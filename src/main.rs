extern crate clap;

use std::fs::File;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::io::SeekFrom;
use walkdir::WalkDir;
use std::path::PathBuf;
use clap::Parser;
use clap::arg;
use rayon::prelude::*;

#[derive(Parser)]
struct Args {
    #[arg(
        short = 'i',
        long = "input-dir",
        value_name = "DIR",
        help = "Input directory containing .ncw files"
    )]
    input_dir: PathBuf,

    #[arg(
        short = 'o',
        long = "output-dir",
        value_name = "DIR",
        help = "Output directory (defaults to same as input file)"
    )]
    output_dir: Option<PathBuf>
}

fn get_u16<T: Read>(f: &mut T) -> u16 {
    let mut buffer = [0; 2];
    f.read_exact(&mut buffer).unwrap();
    u16::from_le_bytes(buffer)
}

fn get_u32<T: Read>(f: &mut T) -> u32 {
    let mut buffer = [0; 4];
    f.read_exact(&mut buffer).unwrap();
    u32::from_le_bytes(buffer)
}

fn get_i32<T: Read>(f: &mut T) -> i32 {
    let mut buffer = [0; 4];
    f.read_exact(&mut buffer).unwrap();
    i32::from_le_bytes(buffer)
}

fn seek<T: std::io::Seek + Read>(f: &mut T, n: usize) -> () {
    f.seek(SeekFrom::Start(n as u64)).unwrap();
}

fn main() -> Result<(), std::io::Error> {
    let matches = Args::parse();
    let input_dir = matches.input_dir;
    let output_dir = matches.output_dir;

     let ncw_paths: Vec<_> = WalkDir::new(input_dir.clone())
     .into_iter()
     .filter_entry(|p| {
         let file_name = p.file_name().to_str().unwrap();
         println!("{}", file_name);
         p.file_type().is_dir() || (file_name.ends_with(".ncw"))
     })
     .filter_map(|f| f.ok())
     .filter(|f| f.file_type().is_file())
     .collect();

    ncw_paths.par_iter().for_each(|path| {
        println!("opening {}...", path.path().to_str().unwrap());
        let mut _f = File::open(path.path()).unwrap();
        let f = &mut _f;
        seek(f, 0x8);
        let num_channels = get_u16(f) as u32;
        let original_bitdepth = get_u16(f) as u32;
        let sample_rate = get_u32(f);
        let sample_count = get_u32(f);
        let _ = get_u32(f);
        let first_frame = get_u32(f);
        let _frame_data_len = get_u32(f);
        // series of bitpacked frames
        let num_frames = (first_frame - 0x78) / 4;
        let mut frames = Vec::<(u32, u32)>::new();
        for i in 0..num_frames - 1 {
            seek(f, (0x78 + i * 4) as usize);
            let start = get_u32(f);
            let end = get_u32(f);
            frames.push((start + first_frame, end - start - 0x10));
        }

        let mut sidemid_flags = Vec::new();
        let mut samples = Vec::new();
        for _i in 0..num_channels {
            samples.push(Vec::new());
        }
        // this format gives each frame a unique bit depth, and a left-right vs mid-side encoding flag
        // as far as I can tell, all frames are 512 samples long, regardless of byte count
        // I have no idea how the left-right vs mid-side encoding flag interacts with non-stereo sources
        for (start, _len) in frames.iter() {
            seek(f, *start as usize);
            for c in 0..num_channels as usize {
                get_i32(f);
                let start_sample = get_i32(f);
                let bits_per_sample = get_u16(f);
                let sidemid_flag = get_u16(f);
                if c == 0 {
                    sidemid_flags.push(sidemid_flag);
                }
                get_i32(f);

                let buffsize = bits_per_sample as u32 * 512 / 8;

                // lambda for pulling bits from bitpacked buffer
                let mut buf = vec![0u8; buffsize as usize];
                f.read(&mut buf).unwrap();
                let mut bit_offset = 0u8;
                let mut buf_offset = 0u64;
                let get_bit = |buf: &Vec<u8>, bit_offset: &mut u8, buf_offset: &mut u64| {
                    if *buf_offset as usize >= buf.len() {
                        panic!("overflow");
                    }
                    let r = ((buf[*buf_offset as usize]) >> (*bit_offset)) & 1;
                    *bit_offset += 1;
                    if *bit_offset >= 8 {
                        *buf_offset += 1;
                        *bit_offset -= 8;
                    }
                    return r as u32;
                };

                // decode samples
                let mut sample : i32 = start_sample;
                samples[c].push(sample as f32 / (2_i32.pow(original_bitdepth - 1)) as f32);
                for _i in 0..512 - 1 {
                    let mut delta = 0u32;
                    let mut first_bit = 0;
                    for j in 0..bits_per_sample {
                        let bit = get_bit(&buf, &mut bit_offset, &mut buf_offset);
                        delta |= bit << j;
                        if j + 1 == bits_per_sample {
                            first_bit = bit;
                        }
                    }
                    delta |= !((1 << (bits_per_sample)) - 1) * first_bit;
                    sample = sample + delta as i32;

                    samples[c].push(sample as f32 / (2_i32.pow(original_bitdepth - 1)) as f32);
                }
                // advance to frame alignment
                while buf_offset % 0x10 > 0 {
                    buf_offset += 1;
                }
            }
        }

        let out_filename = if let Some(out) = &output_dir {
            out.as_path().to_str().unwrap().to_string() + path.file_name().to_str().unwrap()
        } else {
            path.path().to_str().unwrap().into()
        }.replace(".ncw", ".wav");

        let mut out_file = BufWriter::new(File::create(&out_filename).unwrap());
        let bytes_per_sample = 4usize;
        out_file.write(b"RIFF").unwrap();
        out_file.write(&((samples[0].len() * bytes_per_sample + 0x24 + 0xC) as u32).to_le_bytes()).unwrap();
        out_file.write(b"WAVEfmt ").unwrap();
        out_file.write(&16u32.to_le_bytes()).unwrap();
        out_file.write(&3u16.to_le_bytes()).unwrap();
        out_file.write(&(num_channels as u16).to_le_bytes()).unwrap();
        out_file.write(&sample_rate.to_le_bytes()).unwrap();
        out_file
            .write(&(sample_rate * bytes_per_sample as u32 * num_channels as u32).to_le_bytes()).unwrap();
        out_file.write(&(bytes_per_sample as u16).to_le_bytes()).unwrap();
        out_file.write(&(bytes_per_sample as u16 * 8).to_le_bytes()).unwrap();
        out_file.write(b"fact").unwrap();
        out_file.write(&4u32.to_le_bytes()).unwrap();
        out_file.write(&(samples[0].len() as u32).to_le_bytes()).unwrap();
        out_file.write(b"data").unwrap();
        out_file.write(
            &((samples[0].len() * bytes_per_sample * num_channels as usize) as u32).to_le_bytes(),
        ).unwrap();
        for i in 0..sample_count as usize {
            if sidemid_flags[i / 512] == 0 {
                for c in 0..num_channels as usize {
                    out_file.write(&samples[c][i].to_le_bytes()).unwrap();
                }
            } else {
                let mid = &samples[0][i];
                let side = &samples[1][i];
                let left = mid + side;
                let right = mid - side;
                out_file.write(&left.to_le_bytes()).unwrap();
                out_file.write(&right.to_le_bytes()).unwrap();
            }
        }
        println!("wrote {}", out_filename);
    });
    Ok(())
}
