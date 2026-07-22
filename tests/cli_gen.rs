use clap_complete::{Shell, generate};

// The production CLI uses this runtime enum. The source inclusion below keeps
// this integration test focused on the exact derive-based command tree while
// avoiding either optional conversion backend.
#[allow(dead_code)]
mod backend {
    use clap::ValueEnum;

    #[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
    pub enum OutputFormat {
        #[value(name = "geojson")]
        GeoJson,

        #[value(name = "geojson-seq")]
        GeoJsonSeq,
    }
}

#[allow(dead_code)]
#[path = "../src/cli.rs"]
mod cli;

fn completion(shell: Shell) -> Vec<u8> {
    let mut output = Vec::new();
    generate(shell, &mut cli::command(), "dwg2geo", &mut output);
    output
}

fn assert_completion(output: &[u8]) {
    assert!(!output.is_empty());
    let text = String::from_utf8_lossy(output);
    assert!(text.contains("dwg2geo"), "{text}");
    for subcommand in ["convert", "inspect", "doctor"] {
        assert!(text.contains(subcommand), "missing {subcommand}: {text}");
    }
}

#[test]
fn bash_completion_contains_command_tree() {
    assert_completion(&completion(Shell::Bash));
}

#[test]
fn zsh_completion_contains_command_tree() {
    assert_completion(&completion(Shell::Zsh));
}

#[test]
fn man_page_contains_name_and_about_text() {
    let mut output = Vec::new();
    clap_mangen::Man::new(cli::command())
        .render(&mut output)
        .expect("render man page");

    assert!(!output.is_empty());
    let text = String::from_utf8_lossy(&output);
    assert!(text.to_ascii_lowercase().contains("dwg2geo"), "{text}");
    assert!(
        text.contains("Convert engineering DWG drawings to auditable GeoJSON"),
        "{text}"
    );
}
