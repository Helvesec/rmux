mod preset;
mod runner;

use rmux_proto::RmuxError;

use super::settings::WebShareSettings;

pub(crate) use runner::TunnelHandle;

#[derive(Debug)]
pub(crate) struct TunnelInfo {
    pub(crate) handle: TunnelHandle,
    pub(crate) provider: String,
    pub(crate) public_url: String,
}

pub(crate) async fn start_provider(
    name: &str,
    settings: &WebShareSettings,
) -> Result<TunnelInfo, RmuxError> {
    let preset = preset::load(name)?;
    runner::start(preset, settings).await
}

#[cfg(test)]
mod tests {
    use super::preset::{available_from, parse, PresetSource};

    #[test]
    fn embedded_presets_are_valid() {
        for (name, content) in super::preset::embedded() {
            parse(name, PresetSource::Embedded, content).expect("embedded preset parses");
        }
    }

    #[test]
    fn available_presets_are_sorted_and_unique() {
        let names = available_from([("b".to_owned(), ""), ("a".to_owned(), "")], Vec::new());
        assert_eq!(names, vec!["a", "b"]);
    }
}
