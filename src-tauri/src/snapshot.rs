//! Grabs Plouk's own window as pixels (X11) so the options transition can bend
//! and radially blur the real interface on the GPU. Only Plouk's window is
//! read, found through the window manager's client list by process id. Where
//! this isn't possible (e.g. Wayland) it errors and the page falls back to a
//! CSS-only warp.
//!
//! Output: width, height, depth as little-endian u32s, then BGRA rows.

#[cfg(target_os = "linux")]
pub fn capture() -> Result<Vec<u8>, String> {
    use x11rb::{
        connection::Connection,
        protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat},
    };

    let err = |e: &dyn std::fmt::Display| e.to_string();
    let (conn, screen) = x11rb::connect(None).map_err(|e| err(&e))?;
    let root = conn.setup().roots[screen].root;
    let atom = |name: &[u8]| -> Result<u32, String> {
        Ok(conn.intern_atom(false, name).map_err(|e| err(&e))?.reply().map_err(|e| err(&e))?.atom)
    };
    let client_list = atom(b"_NET_CLIENT_LIST")?;
    let wm_pid = atom(b"_NET_WM_PID")?;

    let windows: Vec<u32> = conn
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 4096)
        .map_err(|e| err(&e))?
        .reply()
        .map_err(|e| err(&e))?
        .value32()
        .map(|v| v.collect())
        .unwrap_or_default();

    let me = std::process::id();
    let mut best: Option<(u32, u16, u16, u32)> = None;
    for w in windows {
        let pid = conn
            .get_property(false, w, wm_pid, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()));
        if pid != Some(me) {
            continue;
        }
        let Some(geom) = conn.get_geometry(w).ok().and_then(|c| c.reply().ok()) else { continue };
        let area = geom.width as u32 * geom.height as u32;
        if best.map_or(true, |b| area > b.3) {
            best = Some((w, geom.width, geom.height, area));
        }
    }
    let (window, width, height, _) = best.ok_or("Plouk's window wasn't found")?;

    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, width, height, !0)
        .map_err(|e| err(&e))?
        .reply()
        .map_err(|e| err(&e))?;
    let expected = width as usize * height as usize * 4;
    if image.data.len() < expected {
        return Err("Unsupported window pixel format".into());
    }

    let mut out = Vec::with_capacity(12 + expected);
    out.extend_from_slice(&(width as u32).to_le_bytes());
    out.extend_from_slice(&(height as u32).to_le_bytes());
    out.extend_from_slice(&(image.depth as u32).to_le_bytes());
    out.extend_from_slice(&image.data[..expected]);
    Ok(out)
}

#[cfg(not(target_os = "linux"))]
pub fn capture() -> Result<Vec<u8>, String> {
    Err("Window snapshots are only supported on Linux/X11".into())
}
