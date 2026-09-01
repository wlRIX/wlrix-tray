// SPDX-License-Identifier: GPL-3.0-or-later
//! wlRIX application indicator tray.
//!
//! Hosts the status icons background programs publish over `org.kde.StatusNotifierItem`, in a
//! small dock in the corner of the desktop -- IRIX-style, bottom left by default.
//!
//! Started by `wlrix-session` after `wlrix-desktop`, which is load-bearing: both are on the
//! wlr-layer-shell bottom layer, and the one that maps later sorts above. It is a Wayland client
//! and a D-Bus service at once; see [`wlrix_tray::ui`] and [`wlrix_tray::sni`].

fn main() {
    // No option here combines with another except `--replace`, so the arguments are walked and
    // the first one that ends the program wins.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut replace = false;
    let mut rest = args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!(
                    "wlrix-tray {}\n\n\
                     The application indicator tray for the wlRIX desktop. Hosts\n\
                     org.kde.StatusNotifierItem clients -- fcitx5, Steam and the like.\n\
                     Started by wlrix-session; needs a running compositor with\n\
                     wlr-layer-shell and a session D-Bus.\n\n\
                     Usage: wlrix-tray [options]\n\n\
                     Options:\n  \
                       --replace              take org.kde.StatusNotifierWatcher from\n                         \
                                              whatever holds it, and carry on\n  \
                       --check-config <path>  say whether that file would be accepted, exit\n  \
                       -h, --help             this message\n  \
                       -V, --version          print the version\n\n\
                     Settings live in ~/.config/wlrix/tray.toml, or /etc/wlrix/tray.toml.",
                    env!("CARGO_PKG_VERSION")
                );
                return;
            }
            "--version" | "-V" => {
                println!("wlrix-tray {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            // Answers a question about a file rather than doing anything with it, so it needs no
            // compositor, no bus, and starts nothing. `wlrix-settings-daemon` runs this against a
            // candidate file before renaming it into place, which is what stops a settings app
            // from writing a `tray.toml` this program would refuse.
            "--check-config" => {
                let Some(path) = rest.next() else {
                    eprintln!("wlrix-tray: --check-config needs a path");
                    std::process::exit(2);
                };
                if let Err(why) = wlrix_tray::config::check(std::path::Path::new(path)) {
                    eprintln!("{why}");
                    std::process::exit(1);
                }
                return;
            }
            "--replace" => replace = true,
            other => {
                eprintln!("wlrix-tray: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    if let Err(err) = wlrix_tray::ui::run(replace) {
        eprintln!("wlrix-tray: {err}");
        std::process::exit(1);
    }
}
