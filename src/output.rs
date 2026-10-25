use anyhow::Result;
use colored_json::{ColorMode, ColoredFormatter};
use serde_json::Value;
use serde_json::ser::PrettyFormatter;

const INDENT: &[u8] = b"    ";

pub(crate) fn print_json(value: &Value) -> Result<()> {
    let text = format_json(value, ColorMode::default())?;
    println!("{text}");

    Ok(())
}

fn format_json(value: &Value, color_mode: ColorMode) -> Result<String> {
    let formatter = ColoredFormatter::new(PrettyFormatter::with_indent(INDENT));

    Ok(formatter.to_colored_json(value, color_mode)?)
}

pub(crate) fn format_help(text: &str) -> String {
    let mut output = String::new();

    for line in text.split_inclusive('\n') {
        let content = line.trim_start_matches(' ');
        let indentation = line.len() - content.len();
        output.push_str(&" ".repeat(indentation.div_ceil(INDENT.len()) * INDENT.len()));
        output.push_str(
            &content
                .replace(
                    "Print this message or the help of the given subcommand(s)",
                    "Print this message or the help of the given subcommands.",
                )
                .replace(
                    "Print help (see a summary with '-h')",
                    "Print help. Use -h for a summary.",
                )
                .replace("Print help\n", "Print help.\n")
                .replace("Print version\n", "Print version.\n"),
        );
    }

    output
}

#[cfg(test)]
mod tests {
    use colored_json::ColorMode;
    use serde_json::json;

    use super::{format_help, format_json};

    #[test]
    fn json_preserves_values_and_indents_nested_structures_with_four_spaces() {
        let value = json!({"items": [{"body": "café\n\t\"quoted\"", "active": true, "count": 2, "scope": null}]});
        let plain = format_json(&value, ColorMode::Off).unwrap();
        let colored = format_json(&value, ColorMode::On).unwrap();

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&plain).unwrap(),
            value
        );
        assert!(plain.contains("\n    \"items\": [\n        {\n            \"active\": true,"));
        assert!(!plain.contains('\u{1b}'));
        assert!(colored.contains('\u{1b}'));

        let mut uncolored = String::new();
        let mut is_escape = false;

        for character in colored.chars() {
            if character == '\u{1b}' {
                is_escape = true;
            } else if is_escape {
                is_escape = character != 'm';
            } else {
                uncolored.push(character);
            }
        }

        assert_eq!(uncolored, plain);
    }

    #[test]
    fn help_indents_sections_and_continuations_without_changing_identifiers() {
        let text = "Options:\n  --key <KEY>  Print help\n      continuation\n  -V, --version  Print version\n";

        assert_eq!(
            format_help(text),
            "Options:\n    --key <KEY>  Print help.\n        continuation\n    -V, --version  Print version.\n",
        );
    }
}
