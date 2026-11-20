//! Read the existing manifest and retain the checker before its own launch.
use super::native_windows_process::{
    capacity, now_ns, same_path, sha, sha_bytes, verify_stamp, NativeError, Result,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) type Environment = BTreeMap<OsString, OsString>;
static NEXT_BOUNDARY: AtomicU64 = AtomicU64::new(0);
pub(super) fn string<'a>(record: &'a Value, key: &str) -> Result<&'a str> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(NativeError::Contract(
            "native receipt lacks a required string",
        ))
}
pub(super) fn variable<'a>(env: &'a Environment, key: &str) -> Result<&'a str> {
    env.iter()
        .find(|(name, _)| name.as_encoded_bytes().eq_ignore_ascii_case(key.as_bytes()))
        .and_then(|(_, s)| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or(NativeError::Contract(
            "native child environment lacks a required binding",
        ))
}
pub(super) fn write_once(path: &Path, record: &Value) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, record)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn canonical(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(NativeError::Contract(
            "native input is not an absolute unredirected path",
        ));
    }
    let resolved = path.canonicalize()?;
    if !same_path(path, &resolved) {
        return Err(NativeError::Contract("native input path redirected"));
    }
    Ok(resolved)
}
fn files(record: &Value) -> Result<&serde_json::Map<String, Value>> {
    record
        .get("files")
        .and_then(Value::as_object)
        .ok_or(NativeError::Contract(
            "native manifest has no retained files",
        ))
}
pub(super) fn verify_files(record: &Value) -> Result<()> {
    for (name, row) in files(record)? {
        let path = Path::new(name);
        let object = Path::new(string(row, "object")?);
        let expected = string(row, "sha256")?;
        canonical(object)?;
        if !same_path(&canonical(path)?, Path::new(string(row, "resolved")?))
            || sha(path)? != expected
            || sha(object)? != expected
        {
            return Err(NativeError::Contract(
                "native retained bytes or alias changed",
            ));
        }
        if let Some(stamp) = row.get("windows_stamp") {
            verify_stamp(path, stamp)?;
        }
    }
    Ok(())
}
pub(super) fn qualified_file(record: &Value, path: &Path) -> Result<String> {
    canonical(path)?;
    let row = files(record)?
        .iter()
        .find(|(key, _)| same_path(Path::new(key), path))
        .map(|(_, row)| row)
        .ok_or(NativeError::Contract(
            "checker input lacks before-use qualification",
        ))?;
    let expected = string(row, "sha256")?;
    if sha(path)? != expected {
        return Err(NativeError::Contract("qualified checker input changed"));
    }
    Ok(expected.to_owned())
}
pub(super) struct Manifest {
    pub record: Value,
    pub path: PathBuf,
    pub digest: String,
}
impl Manifest {
    pub fn read(env: &Environment) -> Result<Self> {
        let path = PathBuf::from(variable(env, "RMUX_NATIVE_INPUT_MANIFEST")?);
        let digest = variable(env, "RMUX_NATIVE_INPUT_MANIFEST_SHA256")?.to_owned();
        canonical(&path)?;
        let raw = fs::read(&path)?;
        if sha_bytes(&raw)? != digest {
            return Err(NativeError::Contract("native manifest digest changed"));
        }
        let record: Value = serde_json::from_slice(&raw)?;
        if string(&record, "os")? != "windows"
            || !["sdk", "proto"].contains(&string(&record, "cell")?)
            || variable(env, "RMUX_IDENTITY_HEAD")? != string(&record, "head")?
            || !same_path(
                Path::new(variable(env, "RMUX_IDENTITY_SOURCE")?),
                Path::new(string(&record, "source")?),
            )
        {
            return Err(NativeError::Contract(
                "native consumer belongs to another candidate",
            ));
        }
        canonical(Path::new(string(&record, "source")?))?;
        canonical(Path::new(string(&record, "audit")?))?;
        verify_files(&record)?;
        Ok(Self {
            record,
            path,
            digest,
        })
    }
    pub fn check(&self, env: &Environment) -> Result<()> {
        let actual = Self::read(env)?;
        if actual.path != self.path || actual.digest != self.digest || actual.record != self.record
        {
            return Err(NativeError::Contract(
                "native launch manifest changed at the boundary",
            ));
        }
        Ok(())
    }
    pub fn bootstrap(&self, python: &Path, script: &Path) -> Result<(PathBuf, Value)> {
        // The checker and all imports are already immutable manifest inputs.
        // Copy just these two launch inputs into the existing boundary layout.
        let root = Path::new(string(&self.record, "audit")?);
        let reserve = self.record["reserve_bytes"]
            .as_u64()
            .filter(|bytes| *bytes > 0)
            .ok_or(NativeError::Contract("native retention reserve missing"))?;
        capacity(root, reserve, 0)?;
        let boundary = root.join(format!(
            "boundary-rust-{}-{}-{}",
            std::process::id(),
            now_ns(),
            NEXT_BOUNDARY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&boundary)?;
        let objects = boundary.join("before-objects");
        fs::create_dir(&objects)?;
        let mut retained = serde_json::Map::new();
        for path in [python, script] {
            let digest = qualified_file(&self.record, path)?;
            let object = objects.join(&digest);
            capacity(root, reserve, fs::metadata(path)?.len())?;
            if !object.exists() {
                fs::copy(path, &object)?;
            }
            if sha(&object)? != digest || sha(path)? != digest {
                return Err(NativeError::Contract(
                    "checker bytes changed during retention",
                ));
            }
            retained.insert(
                path.to_string_lossy().into_owned(),
                json!({"resolved":path,
                "sha256":digest,"object":object,"size":fs::metadata(path)?.len()}),
            );
            let mut permissions = fs::metadata(&object)?.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&object, permissions)?;
        }
        let record = json!({"os":"windows","cell":self.record["cell"],"case":self.record["case"],
            "head":self.record["head"],"source":self.record["source"],"at_ns":now_ns(),
            "manifest":self.path,"manifest_sha256":self.digest,"files":retained,
            "executable":python,"executable_sha256":qualified_file(&self.record,python)?});
        write_once(&boundary.join("before.json"), &record)?;
        Ok((boundary, record))
    }
    pub fn launch_boundary(&self, record: Value) -> Result<(PathBuf, Value)> {
        for key in ["head", "source", "case", "cell"] {
            if record[key] != self.record[key] {
                return Err(NativeError::Contract(
                    "checker returned another native boundary",
                ));
            }
        }
        if record["manifest"] != json!(self.path) || record["manifest_sha256"] != self.digest {
            return Err(NativeError::Contract(
                "checker returned another native session",
            ));
        }
        let object = files(&record)?
            .values()
            .next()
            .ok_or(NativeError::Contract("empty native launch boundary"))?;
        let boundary = Path::new(string(object, "object")?)
            .parent()
            .and_then(Path::parent)
            .ok_or(NativeError::Contract(
                "native boundary object has no parent",
            ))?
            .to_owned();
        let audit = canonical(Path::new(string(&self.record, "audit")?))?;
        if !canonical(&boundary)?.starts_with(&audit)
            || serde_json::from_slice::<Value>(&fs::read(boundary.join("before.json"))?)? != record
        {
            return Err(NativeError::Contract(
                "native checker boundary differs from stored receipt",
            ));
        }
        verify_files(&record)?;
        Ok((boundary, record))
    }
}
