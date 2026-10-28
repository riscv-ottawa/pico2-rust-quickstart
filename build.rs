use std::path::PathBuf;

fn main() {
    // Put the linker script where the linker can find it.
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::copy("rp235x_riscv.x", out.join("rp235x_riscv.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=rp235x_riscv.x");

    linker_be_nice();
    // Turn off page alignment of sections, which saves flash space.
    println!("cargo:rustc-link-arg=--nmagic");
    println!("cargo:rustc-link-arg=-Trp235x_riscv.x");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `rp235x_riscv.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("_embassy_time_") => {
                    eprintln!();
                    eprintln!(
                        "💡 No `embassy-time` driver found - make sure the crate's `time_driver` module is linked in"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
