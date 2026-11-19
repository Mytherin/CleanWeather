//! Value histogram of a radar file's image: `cargo run --example histogram -- file.h5`
use hdf5_reader::Hdf5File;
fn main() {
    let f = Hdf5File::open(std::env::args().nth(1).unwrap()).unwrap();
    let v: Vec<u8> = f.dataset("/image1/image_data").unwrap().read_array::<u8>().unwrap().iter().copied().collect();
    let mut h = [0usize; 256];
    v.iter().for_each(|&x| h[x as usize] += 1);
    for (pv, n) in h.iter().enumerate().filter(|(_, n)| **n > 0) {
        if pv < 70 || pv > 250 || pv % 10 == 0 { println!("PV {pv:3} ({:6.1} dBZ): {n}", pv as f32 * 0.5 - 32.0); }
    }
    // Where are the zeros? Interior vs edge: count zeros with all 4 neighbours valid (not 255).
    let (w, hh) = (700, 765);
    let mut interior = 0;
    for y in 1..hh-1 { for x in 1..w-1 { let i = y*w+x; if v[i]==0 && [v[i-1],v[i+1],v[i-w],v[i+w]].iter().all(|&n| n!=255 && n!=0) { interior += 1; } } }
    println!("isolated zeros (neighbours all valid & nonzero): {interior}");
}
