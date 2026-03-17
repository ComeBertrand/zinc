use clap::CommandFactory;
use std::env;
use std::fs;

include!("src/cli.rs");

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Generate shell completions
    let completions_dir = out_dir.join("completions");
    fs::create_dir_all(&completions_dir).unwrap();

    let mut cmd = Cli::command();
    for shell in [
        clap_complete::Shell::Bash,
        clap_complete::Shell::Zsh,
        clap_complete::Shell::Fish,
    ] {
        clap_complete::generate_to(shell, &mut cmd, "zinc", &completions_dir).unwrap();
    }

    // Generate man page
    let man_dir = out_dir.join("man");
    fs::create_dir_all(&man_dir).unwrap();

    let cmd = Cli::command();
    let man = clap_mangen::Man::new(cmd);
    let mut buf = Vec::new();
    man.render(&mut buf).unwrap();
    fs::write(man_dir.join("zinc.1"), buf).unwrap();

    println!("cargo:rerun-if-changed=src/cli.rs");
}
