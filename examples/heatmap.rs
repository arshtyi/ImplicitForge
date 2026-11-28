use implicit_forge::{
    render::{Camera, Params, heatmap},
    scene::demo,
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
    let params = Params::new(&scene, Camera::default(), w, h);
    for (name, frame) in [
        (
            "heatmap-baseline.ppm",
            heatmap::<false>(&scene, &params, true, &|| false).unwrap(),
        ),
        (
            "heatmap-pruned.ppm",
            heatmap::<true>(&scene, &params, true, &|| false).unwrap(),
        ),
    ] {
        let mut out = BufWriter::new(File::create(dir.join(name))?);
        write!(out, "P6\n{w} {h}\n255\n")?;
        for pixel in frame.pixels {
            out.write_all(&pixel.to_be_bytes()[1..])?;
        }
        out.flush()?;
    }
    Ok(())
}
