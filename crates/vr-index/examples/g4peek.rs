//! Debug: textures and sprites of a G4TX read from the game (`g4peek <game> <path>`).
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let src = vr_index::GameSource::open(&a[0], Default::default()).unwrap();
    let b = src.read(&a[1]).unwrap();
    let tx = g4_texture::G4tx::parse(&b).unwrap();
    for t in &tx.textures { let i = t.dds_info().unwrap(); println!("tex {} {}x{} mips {}", t.name, i.width, i.height, i.mip_count); }
    println!("{} sprites", tx.sprites.len());
    for s in tx.sprites.iter().take(12) { println!("  {:?}", s); }
}
