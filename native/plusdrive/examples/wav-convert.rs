use std::{env, fs};
fn main() {
    let mut a = env::args().skip(1);
    let input = a.next().expect("input WAV");
    let output = a.next().expect("output native sample");
    let sample =
        plusdrive_format::wav_to_native(&fs::read(input).expect("read input"), Default::default())
            .expect("convert WAV");
    fs::write(output, &sample.bytes).expect("write output");
    println!(
        "src_rate={} channels={} src_frames={} frames={} data_len={}",
        sample.info.src_rate,
        sample.info.channels,
        sample.info.src_frames,
        sample.info.frames,
        sample.info.data_len
    );
}
