use std::process::Command;

#[test]
fn help_uses_four_space_indentation_and_complete_descriptions() {
    for arguments in [
        vec!["--help"],
        vec!["add", "--help"],
        vec!["taxonomy", "--help"],
        vec!["help", "add"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_semmem"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.lines().next().unwrap().ends_with('.'), "{text}");

        for line in text.lines() {
            let indentation = line.len() - line.trim_start_matches(' ').len();
            assert_eq!(indentation % 4, 0, "{line:?}");
        }

        assert!(text.contains("Print help."), "{text}");
        assert!(!text.contains("; requires"), "{text}");
    }
}

#[test]
fn argument_errors_keep_their_failure_status_and_use_four_space_indentation() {
    let output = Command::new(env!("CARGO_BIN_EXE_semmem"))
        .args(["purge", "example"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(text.contains("--expected-version"), "{text}");
    assert!(text.contains("--confirm"), "{text}");

    for line in text.lines() {
        let indentation = line.len() - line.trim_start_matches(' ').len();
        assert_eq!(indentation % 4, 0, "{line:?}");
    }
}
