// examples/stream_bench.rs

use std::fs::File;
use std::io::Write;

use env_logger;

use rhinostream::{Context, Packet};
use rhinostream::filter::new_nv12_filter;
use rhinostream::processor::nvenc::{NvEnc, NvencConfig};
use rhinostream::source::DxDesktopDuplication;
use rhinostream::stream::{RhinoStream, SignalType};

fn main() {
    let _ = env_logger::init();

    let mut video_file = File::create("video.hevc").unwrap();
    let mut ctx = Context::None;
    let src = DxDesktopDuplication::new("--screen 0".parse().unwrap(), &mut ctx).unwrap();
    let filter = new_nv12_filter("-c rgb -r 2560x1440".parse().unwrap(), &mut ctx).unwrap();
    let config: NvencConfig = "-p p4 --profile auto --multi-pass disabled --aq disabled -t \
        low_latency -r 1920x1080 --codec hevc --color argb -b 10000000 -f 60".parse().unwrap();
    let processor = NvEnc::new(&mut ctx, &config).unwrap();

    let mut stream = RhinoStream::new(src, filter, processor).unwrap();
    let mut packet = Packet::new();
    for i in 0..10000 {
        stream.get_next_frame(&mut packet).unwrap();
        if i == 10 {
            stream.signal(SignalType::Processor(1));
        }
        println!("encode latency: {} μs, size: {} bytes, {:?}", packet.encode_time.as_micros(), packet.data.len(), &packet.data[0..5]);
        video_file.write_all(&packet.data).unwrap();
    }
}