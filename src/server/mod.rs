pub mod autodetect;
pub(crate) mod client_accept;
pub(crate) mod client_transport;
pub(crate) mod clients;
pub(crate) mod clipboard_image;
pub(crate) mod handoff;
pub mod headless;
pub(crate) mod keybindings;
pub(crate) mod notifications;
pub(crate) mod render_stream;
pub mod socket_paths;
pub(crate) mod terminal_attach;

#[cfg(test)]
mod test_support {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Child;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct ProbeDirectory(PathBuf);

    impl Drop for ProbeDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct ProbeChild(Child);

    impl Drop for ProbeChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub(super) fn assert_detached_launch(spawn: impl FnOnce(&Path) -> std::io::Result<Child>) {
        let root = std::env::var_os("ZYNK_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = ProbeDirectory(root.join(format!(
            "daemon-session-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        let exe = dir.0.join("probe");
        std::fs::write(
            &exe,
            "#!/bin/sh\nps -o pid=,sid=,pgid= -p \"$$\" > \"$0.ids\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = ProbeChild(spawn(&exe).expect("spawn through the production launcher"));
        let pid = child.0.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "session probe failed: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "session probe did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let observed: Vec<i32> = std::fs::read_to_string(dir.0.join("probe.ids"))
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse().unwrap())
            .collect();
        assert_eq!(observed.len(), 3, "PID/SID/PGID must all be observed");
        assert_eq!(observed[0], pid, "probe must measure the launched child");
        assert_eq!(observed[1], pid, "daemon child must lead a new session");
        assert_eq!(observed[2], pid, "daemon child must lead its process group");
        assert_ne!(observed[1], unsafe { libc::getsid(0) });
    }
}
