//! Print the groups, datasets and attributes of an HDF5 file:
//! `cargo run -p cleanweather-core --example inspect -- file.h5`
use hdf5_reader::{Hdf5File, group::Group};

fn walk(g: &Group, path: &str) {
    for a in g.attributes().unwrap_or_default() {
        let v = a
            .read_string()
            .map(|s| format!("{s:?}"))
            .or_else(|_| a.read_1d::<f32>().map(|v| format!("{v:?}")))
            .or_else(|_| a.read_1d::<i32>().map(|v| format!("{v:?}")))
            .unwrap_or_else(|e| format!("<{e}>"));
        println!("{path}@{} = {v}", a.name);
    }
    let (groups, datasets) = g.members().unwrap();
    for d in datasets {
        println!("{path}/{} dataset shape={:?}", d.name(), d.shape());
    }
    for c in groups {
        walk(&c, &format!("{path}/{}", c.name()));
    }
}

fn main() {
    let f = Hdf5File::open(std::env::args().nth(1).expect("usage: inspect FILE.h5")).unwrap();
    walk(&f.root_group().unwrap(), "");
}

