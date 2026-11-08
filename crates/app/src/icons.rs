//! Pictures: app icons from their bundles, and a few line icons (Lucide, built into the binary).
use gpui_kit::assets::IconName;
use gpui_kit::*;
use std::path::Path;

gpui_kit::assets::icon_assets!(pub Icons, [Cpu, MemoryStick]);

/// A line icon in `color`.
pub fn line(name: IconName, color: Rgba, size: f32) -> Svg {
    svg().path(name.path()).flex_none().size(px(size)).text_color(color)
}

/// The icon of an app bundle (`/Applications/X.app`) as PNG, the smallest at least 64 px wide (sharp at 32 points on
/// a Retina screen). Read from the bundle's `.icns` file, which holds PNGs: no AppKit, so any thread may call it.
/// Apps whose icon only lives in an asset catalog get none.
pub fn app_icon_png(bundle: &str) -> Option<Vec<u8>> {
    let resources = Path::new(bundle).join("Contents/Resources");
    let plist = std::fs::read(Path::new(bundle).join("Contents/Info.plist")).unwrap_or_default();
    let mut icns: Vec<(std::path::PathBuf, u64)> = std::fs::read_dir(&resources)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "icns"))
        .map(|e| (e.path(), e.metadata().map_or(0, |m| m.len())))
        .collect();
    // The app's icon is named in Info.plist (as CFBundleIconFile, with or without ".icns"); document icons may be
    // named there too, and the app's is the largest of those.
    let named = |p: &Path| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| plist.windows(s.len()).any(|w| w == s.as_bytes()));
    if icns.iter().any(|(p, _)| named(p)) {
        icns.retain(|(p, _)| named(p));
    }
    let (path, _) = icns.into_iter().max_by_key(|(_, len)| *len)?;
    png_from_icns(&std::fs::read(path).ok()?)
}

/// The smallest PNG of at least 64 px in an `.icns` file (types by pixel size; older JPEG 2000 entries are skipped).
fn png_from_icns(data: &[u8]) -> Option<Vec<u8>> {
    const BY_SIZE: [&[u8; 4]; 8] = [b"ic12", b"ic07", b"ic13", b"ic08", b"ic14", b"ic09", b"ic10", b"ic11"];
    if data.get(..4)? != b"icns" {
        return None;
    }
    let mut entries = Vec::new();
    let mut at = 8;
    while at + 8 <= data.len() {
        let kind = &data[at..at + 4];
        let len = u32::from_be_bytes(data[at + 4..at + 8].try_into().ok()?) as usize;
        if len < 8 || at + len > data.len() {
            break;
        }
        let body = &data[at + 8..at + len];
        if body.starts_with(b"\x89PNG") {
            entries.push((kind.to_vec(), body));
        }
        at += len;
    }
    BY_SIZE.iter().find_map(|k| entries.iter().find(|(kind, _)| kind.as_slice() == k.as_slice()).map(|(_, b)| b.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::png_from_icns;

    fn entry(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [kind.as_slice(), &((body.len() + 8) as u32).to_be_bytes(), body].concat()
    }

    #[test]
    fn picks_the_smallest_png_of_64_px_or_more() {
        let png = |tag: u8| [b"\x89PNG".as_slice(), &[tag]].concat();
        let body = [entry(b"ic11", &png(1)), entry(b"ic09", &png(2)), entry(b"ic07", &png(3)), entry(b"it32", b"rgb")].concat();
        let file = [b"icns".as_slice(), &((body.len() + 8) as u32).to_be_bytes(), &body].concat();
        assert_eq!(png_from_icns(&file), Some(png(3)), "ic07 (128 px) beats ic09 (512 px) and ic11 (32 px)");
        assert_eq!(png_from_icns(b"nope"), None);
    }
}
