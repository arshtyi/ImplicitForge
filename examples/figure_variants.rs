use implicit_forge::{
    render::{Camera, Params, cpu},
    scene::{Stamp, demo},
};
use std::{
    env,
    fs::File,
    io::{self, BufWriter, Write},
    path::PathBuf,
};

fn main() -> io::Result<()> {
    let dir = PathBuf::from(
        env::args_os()
            .nth(1)
            .expect("expected PPM output directory"),
    );
    let scene = demo(0, [0.5; 3]);
    let (w, h) = (640, 400);
    let view = Params::new(&scene, Camera::default(), w, h);
    let stamped = demo(0, [0.5; 3]).with_stamp(Stamp::default());
    let stamp_view = Params::new(&stamped, Camera::default(), w, h);
    for (name, scene, params) in [
        ("center", &scene, view.single_sample()),
        ("stamped", &stamped, stamp_view),
    ] {
        let frame = cpu::<true, false>(scene, &params, true, &|| false).unwrap();
        let mut out = BufWriter::new(File::create(dir.join(format!("{name}.ppm")))?);
        write!(out, "P6\n{w} {h}\n255\n")?;
        for pixel in frame.pixels {
            out.write_all(&pixel.to_be_bytes()[1..])?;
        }
        out.flush()?;
    }
    Ok(())
}
