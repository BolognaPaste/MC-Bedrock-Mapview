//! The map page, embedded in the program so it can be served and copied into every map folder.
pub static FILES: &[(&str, &[u8])] = &[
    ("index.html", include_bytes!("../../viewer/index.html")),
    ("viewer.js", include_bytes!("../../viewer/viewer.js")),
    ("map3d.js", include_bytes!("../../viewer/map3d.js")),
    ("viewer.css", include_bytes!("../../viewer/viewer.css")),
    ("leaves.js", include_bytes!("../../viewer/leaves.js")),
];

pub fn get(name: &str) -> Option<&'static [u8]> {
    FILES.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}
