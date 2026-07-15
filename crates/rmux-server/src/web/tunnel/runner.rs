use std::collections::VecDeque;
use std::io::{self, BufRead, Read};
use std::net::SocketAddr;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use rmux_os::process_tree::{ProcessTreeChild, ProcessTreeController};
use rmux_proto::RmuxError;
use tokio::net::{lookup_host, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

use super::preset::{ProcessOutput, TunnelPreset};
use crate::web::origin::validate_public_base_url;
use crate::web::settings::WebShareSettings;
use crate::web::tunnel::TunnelInfo;

const LINE_CHANNEL_CAPACITY: usize = 64;
const ERROR_LINE_LIMIT: usize = 8;
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(5);
const TUNNEL_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(250);
const PUBLIC_ENDPOINT_INITIAL_PROBE_DELAY: Duration = Duration::from_secs(1);
const PUBLIC_ENDPOINT_RETRY_DELAY: Duration = Duration::from_secs(1);
const PUBLIC_ENDPOINT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) struct TunnelHandle {
    provider: String,
    stop_flag: Arc<AtomicBool>,
    stop_tx: Option<oneshot::Sender<()>>,
    _output_task: Option<JoinHandle<()>>,
    _task: JoinHandle<()>,
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        debug!(provider = %self.provider, "stopping web-share tunnel provider");
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }
}

pub(super) async fn start(
    preset: TunnelPreset,
    settings: &WebShareSettings,
) -> Result<TunnelInfo, RmuxError> {
    let regex = Regex::new(&preset.url_pattern).map_err(|error| {
        RmuxError::Server(format!(
            "web-share tunnel preset '{}' has an invalid url_pattern: {error}",
            preset.name
        ))
    })?;
    let ready_regex = preset
        .ready_pattern
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|error| {
            RmuxError::Server(format!(
                "web-share tunnel preset '{}' has an invalid ready_pattern: {error}",
                preset.name
            ))
        })?;
    let program = expand(&preset.program, settings)?;
    let args = preset
        .args
        .iter()
        .map(|arg| expand(arg, settings))
        .collect::<Result<Vec<_>, _>>()?;
    let mut command = Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ProcessTreeChild::spawn(&mut command)
        .map_err(|error| spawn_error(&preset, &program, error))?;
    let stdout = child.child_mut().stdout.take().expect("stdout is piped");
    let stderr = child.child_mut().stderr.take().expect("stderr is piped");
    let (line_tx, line_rx) = mpsc::channel(LINE_CHANNEL_CAPACITY);
    spawn_line_reader(
        "rmux-tunnel-stdout",
        stdout,
        line_tx.clone(),
        ProcessOutput::Stdout,
    );
    spawn_line_reader("rmux-tunnel-stderr", stderr, line_tx, ProcessOutput::Stderr);

    let (stop_tx, stop_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = oneshot::channel();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let handle_stop_flag = Arc::clone(&stop_flag);
    let task = tokio::spawn(async move {
        let mut child_wait = tokio::task::spawn_blocking({
            let stop_flag = stop_flag.clone();
            move || wait_for_tunnel_child(child, stop_flag)
        });
        let status = tokio::select! {
            _ = stop_rx => {
                stop_flag.store(true, Ordering::SeqCst);
                child_wait.await.map_err(|error| io::Error::other(format!("tunnel wait task failed: {error}")))
            }
            status = &mut child_wait => {
                status.map_err(|error| io::Error::other(format!("tunnel wait task failed: {error}")))
            }
        };
        let _ = exit_tx.send(status.and_then(|status| status));
    });
    let mut handle = Some(TunnelHandle {
        provider: preset.name.clone(),
        stop_flag: handle_stop_flag,
        stop_tx: Some(stop_tx),
        _output_task: None,
        _task: task,
    });
    let (public_url, line_rx) =
        match wait_for_url(&preset, &regex, ready_regex.as_ref(), line_rx, exit_rx).await {
            Ok(url) => url,
            Err(error) => {
                drop(handle.take());
                return Err(error);
            }
        };
    let output_task = spawn_output_drain(preset.name.clone(), line_rx);
    if let Some(handle) = &mut handle {
        handle._output_task = Some(output_task);
    }
    if let Err(error) = wait_for_public_endpoint(&preset, &public_url).await {
        drop(handle.take());
        return Err(error);
    }
    info!(
        provider = %preset.name,
        public_url,
        "web_share_tunnel_ready"
    );
    Ok(TunnelInfo {
        handle: handle.expect("handle remains when tunnel starts"),
        provider: preset.name,
        public_url,
    })
}

async fn wait_for_public_endpoint(preset: &TunnelPreset, url: &str) -> Result<(), RmuxError> {
    let (host, port) = public_endpoint(url).map_err(|error| {
        RmuxError::Server(format!(
            "web-share tunnel provider '{}' printed an invalid public URL '{}': {error}",
            preset.name, url
        ))
    })?;
    let wait = async {
        sleep(PUBLIC_ENDPOINT_INITIAL_PROBE_DELAY).await;
        loop {
            if endpoint_accepts_connections(&host, port).await.is_ok() {
                return Ok::<(), ()>(());
            }
            sleep(PUBLIC_ENDPOINT_RETRY_DELAY).await;
        }
    };
    match timeout(Duration::from_secs(preset.ready_timeout_secs), wait)
        .await
        .map_err(|_| {
            RmuxError::Server(format!(
                "web-share tunnel provider '{}' printed '{}' but it did not become reachable within {}s",
                preset.name, url, preset.ready_timeout_secs
            ))
        })? {
        Ok(()) => Ok(()),
        Err(()) => unreachable!("public endpoint wait loop never returns an inner error"),
    }
}

async fn endpoint_accepts_connections(host: &str, port: u16) -> io::Result<()> {
    let addrs = ordered_endpoint_addrs(lookup_host((host, port)).await?);
    let mut last_error = None;
    for addr in addrs {
        match timeout(PUBLIC_ENDPOINT_CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(_stream)) => return Ok(()),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connection attempt timed out",
                ));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "public endpoint did not resolve to any address",
        )
    }))
}

fn ordered_endpoint_addrs(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    let mut ipv4 = Vec::new();
    let mut ipv6 = Vec::new();
    for addr in addrs {
        if addr.is_ipv4() {
            ipv4.push(addr);
        } else {
            ipv6.push(addr);
        }
    }
    ipv4.extend(ipv6);
    ipv4
}

fn public_endpoint(url: &str) -> io::Result<(String, u16)> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing scheme"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, raw_port))
            if !host.is_empty() && raw_port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            let port = raw_port.parse::<u16>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid port: {error}"),
                )
            })?;
            (host, port)
        }
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid authority",
            ));
        }
        None => (authority, default_port(scheme)?),
    };
    if host.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "missing host"));
    }
    Ok((host.to_owned(), port))
}

fn default_port(scheme: &str) -> io::Result<u16> {
    match scheme {
        "http" => Ok(80),
        "https" => Ok(443),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported URL scheme",
        )),
    }
}

fn wait_for_tunnel_child(
    mut child: ProcessTreeChild,
    stop_flag: Arc<AtomicBool>,
) -> io::Result<ExitStatus> {
    let controller = child.controller();
    loop {
        if child.has_exited()? {
            return terminate_descendants_and_wait(&mut child, &controller);
        }
        if stop_flag.load(Ordering::SeqCst) {
            terminate_child_gracefully(&mut child);
            let deadline = Instant::now() + STOP_GRACE_PERIOD;
            loop {
                if child.has_exited()? {
                    return terminate_descendants_and_wait(&mut child, &controller);
                }
                if Instant::now() >= deadline {
                    return terminate_descendants_and_wait(&mut child, &controller);
                }
                thread::sleep(TUNNEL_CHILD_POLL_INTERVAL);
            }
        }
        thread::sleep(TUNNEL_CHILD_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn terminate_child_gracefully(child: &mut ProcessTreeChild) {
    let _ = child.forward_signal(rustix::process::Signal::TERM.as_raw());
}

#[cfg(not(unix))]
fn terminate_child_gracefully(child: &mut ProcessTreeChild) {
    let _ = child.terminate();
}

fn terminate_descendants_and_wait(
    child: &mut ProcessTreeChild,
    controller: &ProcessTreeController,
) -> io::Result<ExitStatus> {
    let terminate_result = controller.terminate();
    let wait_result = child.wait();
    terminate_result.and(wait_result)
}

async fn wait_for_url(
    preset: &TunnelPreset,
    regex: &Regex,
    ready_regex: Option<&Regex>,
    mut lines: mpsc::Receiver<(ProcessOutput, String)>,
    mut exit_rx: oneshot::Receiver<io::Result<std::process::ExitStatus>>,
) -> Result<(String, mpsc::Receiver<(ProcessOutput, String)>), RmuxError> {
    let mut last_lines = VecDeque::new();
    let ready = async {
        let mut found_url = None;
        let mut provider_ready = ready_regex.is_none();
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some((source, line)) = line else {
                        return Err(tunnel_error(preset, "ended before printing a public URL", &last_lines));
                    };
                    remember_line(&mut last_lines, &line);
                    if !preset.url_source.accepts(source) {
                        continue;
                    }
                    if let Some(ready_regex) = ready_regex {
                        provider_ready |= ready_regex.is_match(&line);
                    }
                    if let Some(found) = regex.find(&line) {
                        found_url = Some(validate_public_base_url(found.as_str())?);
                    }
                    if provider_ready {
                        if let Some(url) = found_url.take() {
                            return Ok((url, lines));
                        }
                    }
                }
                status = &mut exit_rx => {
                    let detail = match status {
                        Ok(Ok(status)) => format!("exited with {status} before printing a public URL"),
                        Ok(Err(error)) => format!("failed while waiting for tunnel process: {error}"),
                        Err(_) => "ended before printing a public URL".to_owned(),
                    };
                    return Err(tunnel_error(preset, &detail, &last_lines));
                }
            }
        }
    };
    timeout(Duration::from_secs(preset.ready_timeout_secs), ready)
        .await
        .map_err(|_| tunnel_error(preset, "timed out waiting for a public URL", &last_lines))?
}

fn spawn_line_reader<R>(
    name: &'static str,
    reader: R,
    tx: mpsc::Sender<(ProcessOutput, String)>,
    source: ProcessOutput,
) where
    R: Read + Send + 'static,
{
    let _ = thread::Builder::new().name(name.to_owned()).spawn(move || {
        read_lines(source, reader, tx);
    });
}

fn spawn_output_drain(
    provider: String,
    mut lines: mpsc::Receiver<(ProcessOutput, String)>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((source, line)) = lines.recv().await {
            debug!(
                provider,
                source = ?source,
                line,
                "web-share tunnel provider output"
            );
        }
    })
}

fn read_lines<R>(source: ProcessOutput, reader: R, tx: mpsc::Sender<(ProcessOutput, String)>)
where
    R: Read,
{
    for line in io::BufReader::new(reader).lines() {
        match line {
            Ok(line) => {
                if tx.blocking_send((source, line)).is_err() {
                    return;
                }
            }
            Err(error) => {
                debug!("web-share tunnel output read failed: {error}");
                return;
            }
        }
    }
}

fn remember_line(lines: &mut VecDeque<String>, line: &str) {
    if lines.len() == ERROR_LINE_LIMIT {
        lines.pop_front();
    }
    lines.push_back(line.to_owned());
}

fn tunnel_error(preset: &TunnelPreset, detail: &str, lines: &VecDeque<String>) -> RmuxError {
    let mut message = format!("web-share tunnel provider '{}' {detail}", preset.name);
    if !lines.is_empty() {
        message.push_str(". Last output:\n");
        for line in lines {
            message.push_str("  ");
            message.push_str(line);
            message.push('\n');
        }
    }
    if let Some(hint) = preset.install_hint.as_deref() {
        message.push_str(". ");
        message.push_str(hint);
    }
    RmuxError::Server(message)
}

fn spawn_error(preset: &TunnelPreset, program: &str, error: io::Error) -> RmuxError {
    let mut message = format!(
        "failed to start web-share tunnel provider '{}' with '{}': {error}",
        preset.name, program
    );
    if error.kind() == io::ErrorKind::NotFound {
        if let Some(hint) = preset.install_hint.as_deref() {
            message.push_str(". ");
            message.push_str(hint);
        }
    }
    RmuxError::Server(message)
}

fn expand(value: &str, settings: &WebShareSettings) -> Result<String, RmuxError> {
    let expanded = value
        .replace("{host}", &settings.host)
        .replace("{port}", &settings.port.to_string());
    if expanded.contains('{') || expanded.contains('}') {
        return Err(RmuxError::Server(format!(
            "web-share tunnel preset contains an unknown placeholder in '{value}'"
        )));
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    #[cfg(unix)]
    use std::path::Path;

    #[test]
    fn endpoint_probe_prefers_ipv4_before_ipv6() {
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443);

        let ordered = super::ordered_endpoint_addrs([ipv6, ipv4]);

        assert_eq!(ordered, vec![ipv4, ipv6]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runner_extracts_public_url_and_stops_on_drop() {
        use super::start;
        use crate::web::settings::WebShareSettings;
        use crate::web::tunnel::preset::{TunnelPreset, UrlSource};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind readiness probe listener");
        let port = listener.local_addr().expect("listener addr").port();
        let _listener_task =
            tokio::spawn(
                async move { while let Ok((_stream, _addr)) = listener.accept().await {} },
            );
        let url = format!("http://127.0.0.1:{port}");
        let preset = TunnelPreset {
            name: "test".to_owned(),
            program: "sh".to_owned(),
            args: vec!["-c".to_owned(), format!("printf '%s\\n' {url}; sleep 30")],
            url_pattern: regex::escape(&url),
            ready_pattern: None,
            url_source: UrlSource::Stdout,
            ready_timeout_secs: 5,
            install_hint: None,
        };
        let info = start(preset, &WebShareSettings::default())
            .await
            .expect("tunnel starts");
        assert_eq!(info.public_url, url);
        drop(info);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_tunnel_stops_wrapper_descendants() {
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        use super::start;
        use crate::web::settings::WebShareSettings;
        use crate::web::tunnel::preset::{TunnelPreset, UrlSource};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind readiness probe listener");
        let port = listener.local_addr().expect("listener addr").port();
        let _listener_task =
            tokio::spawn(
                async move { while let Ok((_stream, _addr)) = listener.accept().await {} },
            );
        let root = std::env::temp_dir().join(format!(
            "rmux-tunnel-tree-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create tunnel test root");
        let pid_path = root.join("descendant.pid");
        let url = format!("http://127.0.0.1:{port}");
        let script = format!(
            "sleep 30 & child=$!; printf '%s' \"$child\" > '{}'; printf '%s\\n' '{}'; wait",
            pid_path.display(),
            url
        );
        let preset = TunnelPreset {
            name: "wrapper-tree-test".to_owned(),
            program: "sh".to_owned(),
            args: vec!["-c".to_owned(), script],
            url_pattern: regex::escape(&url),
            ready_pattern: None,
            url_source: UrlSource::Stdout,
            ready_timeout_secs: 5,
            install_hint: None,
        };
        let info = start(preset, &WebShareSettings::default())
            .await
            .expect("tunnel starts");
        let descendant_pid = wait_for_descendant_pid(&pid_path).await;
        assert!(process_exists(descendant_pid), "descendant must start");

        drop(info);

        let deadline = Instant::now() + Duration::from_secs(3);
        while process_exists(descendant_pid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !process_exists(descendant_pid),
            "dropping a tunnel must terminate its provider descendants"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    async fn wait_for_descendant_pid(path: &Path) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(pid) = raw.parse::<i32>() {
                    return pid;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tunnel descendant pid was not published"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    fn process_exists(pid: i32) -> bool {
        rustix::process::Pid::from_raw(pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }
}
