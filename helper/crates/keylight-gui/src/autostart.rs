//! "Start on login": a `.desktop` file in `~/.config/autostart/`.
//!
//! Inside Flatpak the app's own XDG_CONFIG_HOME is sandboxed, so we write to
//! the host path (granted by `--filesystem=xdg-config/autostart:create`).

use std::path::PathBuf;

pub const APP_ID: &str = "io.github.chimi6.limelight-linux-elgato-lights-controller";

pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

fn autostart_dir() -> PathBuf {
    if is_flatpak() {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/var/home"))
            .join(".config/autostart")
    } else {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("autostart")
    }
}

fn autostart_path() -> PathBuf {
    autostart_dir().join(format!("{APP_ID}.desktop"))
}

pub fn enabled() -> bool {
    autostart_path().exists()
}

fn exec_line() -> String {
    if is_flatpak() {
        format!("flatpak run {APP_ID}")
    } else if let Ok(appimage) = std::env::var("APPIMAGE") {
        // Running from an AppImage: point at the image, not the extracted binary.
        appimage
    } else {
        std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("limelight"))
            .display()
            .to_string()
    }
}

pub fn enable() -> Result<(), String> {
    let dir = autostart_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=LimeLight\n\
         Comment=Elgato light controller\n\
         Exec={}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_line()
    );
    if is_flatpak() {
        contents.push_str(&format!("X-Flatpak={APP_ID}\n"));
    }
    std::fs::write(autostart_path(), contents).map_err(|e| e.to_string())
}

pub fn disable() -> Result<(), String> {
    match std::fs::remove_file(autostart_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
