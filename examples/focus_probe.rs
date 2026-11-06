//! Diagnostic: print the frontmost app and (optionally) re-activate a PID.
//!
//!   cargo run --example focus_probe -- [pid]
//!
//! Used to smoke-test `pianomi::focus` without clicking through the GUI.

fn main() {
    let before = pianomi::focus::frontmost_pid();
    println!("frontmost pid: {before}");

    if let Some(arg) = std::env::args().nth(1) {
        let pid: i32 = arg.parse().expect("a numeric pid");
        let ok = pianomi::focus::activate_pid(pid);
        println!("activate_pid({pid}): {ok}");
        std::thread::sleep(std::time::Duration::from_millis(300));
        println!("frontmost pid after: {}", pianomi::focus::frontmost_pid());
    }

    assert!(before > 0, "NSWorkspace returned no frontmost application");
}
