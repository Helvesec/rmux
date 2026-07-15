use std::sync::{Arc, Condvar, Mutex};

use rmux_proto::TerminalSize;

use super::{PopupIoOperation, PopupIoQueue};

#[tokio::test]
async fn popup_io_queue_preserves_write_resize_write_enqueue_order() {
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let watchdog_release = Arc::clone(&release);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let (released, released_cv) = &*watchdog_release;
        *released.lock().expect("I/O watchdog release") = true;
        released_cv.notify_all();
    });
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let callback_observed = Arc::clone(&observed);
    let callback_release = Arc::clone(&release);
    let callback_started = Arc::clone(&started_tx);
    let queue = PopupIoQueue::spawn(move |operation| {
        let label = match operation {
            PopupIoOperation::Write(bytes) => {
                format!("write:{}", String::from_utf8_lossy(&bytes))
            }
            PopupIoOperation::Resize(size) => {
                format!("resize:{}x{}", size.cols, size.rows)
            }
        };
        let first = {
            let mut observed = callback_observed.lock().expect("observed popup I/O");
            observed.push(label);
            observed.len() == 1
        };
        if first {
            if let Some(started_tx) = callback_started.lock().expect("I/O start").take() {
                let _ = started_tx.send(());
            }
            let (released, released_cv) = &*callback_release;
            let mut released = released.lock().expect("I/O release");
            while !*released {
                released = released_cv.wait(released).expect("I/O release wait");
            }
        }
        Ok(())
    });

    let first = queue
        .enqueue(PopupIoOperation::Write(b"a".to_vec()))
        .expect("enqueue first write");
    let started = tokio::time::timeout(std::time::Duration::from_secs(2), started_rx).await;
    let second = queue
        .enqueue(PopupIoOperation::Resize(TerminalSize {
            cols: 41,
            rows: 17,
        }))
        .expect("enqueue resize");
    let third = queue
        .enqueue(PopupIoOperation::Write(b"b".to_vec()))
        .expect("enqueue second write");
    {
        let (released, released_cv) = &*release;
        *released.lock().expect("I/O release") = true;
        released_cv.notify_all();
    }
    started
        .expect("first popup I/O should start")
        .expect("popup I/O start sender should remain connected");

    let (first, second, third) = tokio::join!(first.wait(), second.wait(), third.wait());
    first.expect("first write completes");
    second.expect("resize completes");
    third.expect("second write completes");
    assert_eq!(
        *observed.lock().expect("observed popup I/O"),
        ["write:a", "resize:41x17", "write:b"]
    );
}

#[cfg(windows)]
mod windows {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::super::spawn_popup_windows_teardown;

    fn reader_completion(master: rmux_pty::PtyMaster) -> mpsc::Receiver<()> {
        let reader = master.into_io();
        let (reader_done_tx, reader_done_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0_u8; 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = reader_done_tx.send(());
                        return;
                    }
                    Ok(_) => {}
                }
            }
        });
        reader_done_rx
    }

    #[test]
    fn popup_natural_exit_closes_pseudoconsole_and_releases_reader(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let spawned = ChildCommand::new(r"C:\Windows\System32\cmd.exe")
            .args(["/D", "/C", "exit 0"])
            .size(PtyTerminalSize::new(20, 6))
            .spawn()?;
        let (master, mut child) = spawned.into_parts();
        let reader_done = reader_completion(master);
        let close_child = child.try_clone_for_wait()?;
        assert!(child.wait()?.success());
        spawn_popup_windows_teardown(Some(child), Some(close_child));
        reader_done.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    }

    #[test]
    fn popup_shutdown_drops_job_before_closing_pseudoconsole(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let spawned = ChildCommand::new(r"C:\Windows\System32\cmd.exe")
            .args(["/D", "/Q", "/C", "ping -n 120 127.0.0.1 >NUL"])
            .size(PtyTerminalSize::new(20, 6))
            .spawn()?;
        let (master, child) = spawned.into_parts();
        let reader_done = reader_completion(master);
        let close_child = child.try_clone_for_wait()?;
        spawn_popup_windows_teardown(Some(child), Some(close_child));
        reader_done.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    }
}
