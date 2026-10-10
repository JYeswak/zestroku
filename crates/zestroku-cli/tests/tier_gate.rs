use std::process::Command;

#[test]
fn keypress_is_refused_before_network_dispatch() {
    let output = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .args(["key", "127.0.0.1", "Home"])
        .output()
        .expect("run zestroku key command");

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("not allowed in Open mode"));
}
