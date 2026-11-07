use std::fs;

/// Parent pid from `/proc/<pid>/stat` (field 4; the comm field may contain spaces).
fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 1..];
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

/// True if `ancestor` is a strict ancestor of `pid`.
pub fn is_ancestor(ancestor: u32, pid: u32) -> bool {
    let mut cur = pid;
    while let Some(parent) = parent_of(cur) {
        if parent == ancestor {
            return true;
        }
        if parent <= 1 {
            return false;
        }
        cur = parent;
    }
    false
}

pub fn pid_alive(pid: u32) -> bool {
    fs::metadata(format!("/proc/{pid}")).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn ancestor_walk_finds_grandparent() {
        // sh (child) -> sleep (grandchild); the trailing `;` stops sh exec'ing sleep.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 5; true"])
            .spawn()
            .unwrap();
        let sh_pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let out = Command::new("pgrep")
            .args(["-P", &sh_pid.to_string()])
            .output()
            .unwrap();
        let grandchild: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .lines()
            .next()
            .expect("sh has a child")
            .parse()
            .unwrap();
        let ours = std::process::id();
        assert!(is_ancestor(ours, grandchild));
        assert!(is_ancestor(sh_pid, grandchild));
        assert!(!is_ancestor(grandchild, ours));
        child.kill().ok();
        Command::new("kill")
            .arg(grandchild.to_string())
            .status()
            .ok();
        child.wait().ok();
    }

    #[test]
    fn dead_pid_is_not_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX));
    }
}
