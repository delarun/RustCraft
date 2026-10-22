//! java.util.zip natives: Inflater/Deflater (miniz_oxide), CRC32, Adler32.
//!
//! Streams are kept in a table keyed by the handle returned from `init`.
//! Results are packed as in OpenJDK's Inflater.c / Deflater.c: bits 0–30
//! input consumed, 31–61 output produced, bit 62 finished, bit 63 needs
//! dictionary (inflate) / params applied (deflate).

use super::*;
use crate::heap::ARRAY_BASE_OFFSET;
use miniz_oxide::deflate::core::{CompressorOxide, create_comp_flags_from_zip_params};
use miniz_oxide::inflate::stream::InflateState;
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use std::collections::HashMap;
use std::sync::Mutex;

enum Stream {
    Inflate(Box<InflateState>, DataFormat),
    Deflate { compressor: Box<CompressorOxide>, raw: bool, adler: u32 },
}

static STREAMS: Mutex<(i64, Option<HashMap<i64, Stream>>)> = Mutex::new((0, None));

fn insert(stream: Stream) -> i64 {
    let mut streams = STREAMS.lock().unwrap();
    streams.0 += 1;
    let handle = streams.0;
    streams.1.get_or_insert_with(HashMap::new).insert(handle, stream);
    handle
}

fn with_stream<R>(handle: i64, f: impl FnOnce(&mut Stream) -> R) -> R {
    let mut streams = STREAMS.lock().unwrap();
    f(streams.1.as_mut().and_then(|m| m.get_mut(&handle)).expect("live zip stream"))
}

fn pack(consumed: usize, produced: usize, finished: bool, flag: bool) -> i64 {
    (consumed as i64 & 0x7fff_ffff) | ((produced as i64 & 0x7fff_ffff) << 31) | ((finished as i64) << 62) | ((flag as i64) << 63)
}

/// A byte range of a Java array.
fn bytes(vm: &crate::vm::Vm, array: Ref, offset: i32, len: i32) -> &mut [u8] {
    if len <= 0 {
        return &mut [];
    }
    vm.heap.bytes_mut(array, ARRAY_BASE_OFFSET + offset as u32, len as usize)
}

/// A byte range of native memory.
fn native_bytes<'a>(address: i64, len: i32) -> &'a mut [u8] {
    if len <= 0 {
        return &mut [];
    }
    // SAFETY: the Java side passes a direct buffer region of `len` bytes.
    unsafe { std::slice::from_raw_parts_mut(address as usize as *mut u8, len as usize) }
}

pub(super) fn register(n: &mut Natives) {
    n.add("java/util/zip/Inflater.initIDs()V", no_op);
    n.add("java/util/zip/Inflater.init(Z)J", |_, a| {
        let format = if arg_z(a, 0) { DataFormat::Raw } else { DataFormat::Zlib };
        ret_j(insert(Stream::Inflate(InflateState::new_boxed(format), format)))
    });
    n.add("java/util/zip/Inflater.inflateBytesBytes(J[BII[BII)J", |t, a| {
        let input = bytes(t.vm, arg_ref(a, 3), arg_i(a, 4), arg_i(a, 5)).to_vec();
        let output = bytes(t.vm, arg_ref(a, 6), arg_i(a, 7), arg_i(a, 8));
        inflate(t, arg_j(a, 1), &input, output)
    });
    n.add("java/util/zip/Inflater.inflateBytesBuffer(J[BIIJI)J", |t, a| {
        let input = bytes(t.vm, arg_ref(a, 3), arg_i(a, 4), arg_i(a, 5)).to_vec();
        let output = native_bytes(arg_j(a, 6), arg_i(a, 8));
        inflate(t, arg_j(a, 1), &input, output)
    });
    n.add("java/util/zip/Inflater.inflateBufferBytes(JJI[BII)J", |t, a| {
        let input = native_bytes(arg_j(a, 3), arg_i(a, 5)).to_vec();
        let output = bytes(t.vm, arg_ref(a, 6), arg_i(a, 7), arg_i(a, 8));
        inflate(t, arg_j(a, 1), &input, output)
    });
    n.add("java/util/zip/Inflater.inflateBufferBuffer(JJIJI)J", |t, a| {
        let input = native_bytes(arg_j(a, 3), arg_i(a, 5)).to_vec();
        let output = native_bytes(arg_j(a, 6), arg_i(a, 8));
        inflate(t, arg_j(a, 1), &input, output)
    });
    n.add("java/util/zip/Inflater.getAdler(J)I", |_, a| {
        ret_i(with_stream(arg_j(a, 0), |s| match s {
            Stream::Inflate(state, _) => state.decompressor().adler32().unwrap_or(1) as i32,
            Stream::Deflate { adler, .. } => *adler as i32,
        }))
    });
    n.add("java/util/zip/Inflater.reset(J)V", |_, a| {
        with_stream(arg_j(a, 0), |s| {
            if let Stream::Inflate(state, format) = s {
                state.reset_as(miniz_oxide::inflate::stream::FullReset(*format));
            }
        });
        ret_void()
    });
    n.add("java/util/zip/Inflater.end(J)V", end);
    n.add("java/util/zip/Inflater.setDictionary(J[BII)V", |t, _| {
        Err(t.throw_new("java/lang/UnsupportedOperationException", Some("inflate dictionaries are not supported")))
    });

    n.add("java/util/zip/Deflater.init(IIZ)J", |_, a| {
        let (level, strategy, raw) = (arg_i(a, 0), arg_i(a, 1), arg_z(a, 2));
        let mut compressor = Box::new(CompressorOxide::default());
        configure(&mut compressor, level, strategy, raw);
        ret_j(insert(Stream::Deflate { compressor, raw, adler: 1 }))
    });
    n.add("java/util/zip/Deflater.deflateBytesBytes(J[BII[BIIII)J", |t, a| {
        let input = bytes(t.vm, arg_ref(a, 3), arg_i(a, 4), arg_i(a, 5)).to_vec();
        let output = bytes(t.vm, arg_ref(a, 6), arg_i(a, 7), arg_i(a, 8));
        deflate(arg_j(a, 1), &input, output, arg_i(a, 9), arg_i(a, 10))
    });
    n.add("java/util/zip/Deflater.deflateBytesBuffer(J[BIIJIII)J", |t, a| {
        let input = bytes(t.vm, arg_ref(a, 3), arg_i(a, 4), arg_i(a, 5)).to_vec();
        let output = native_bytes(arg_j(a, 6), arg_i(a, 8));
        deflate(arg_j(a, 1), &input, output, arg_i(a, 9), arg_i(a, 10))
    });
    n.add("java/util/zip/Deflater.deflateBufferBytes(JJI[BIIII)J", |t, a| {
        let input = native_bytes(arg_j(a, 3), arg_i(a, 5)).to_vec();
        let output = bytes(t.vm, arg_ref(a, 6), arg_i(a, 7), arg_i(a, 8));
        deflate(arg_j(a, 1), &input, output, arg_i(a, 9), arg_i(a, 10))
    });
    n.add("java/util/zip/Deflater.deflateBufferBuffer(JJIJIII)J", |_, a| {
        let input = native_bytes(arg_j(a, 3), arg_i(a, 5)).to_vec();
        let output = native_bytes(arg_j(a, 6), arg_i(a, 8));
        deflate(arg_j(a, 1), &input, output, arg_i(a, 9), arg_i(a, 10))
    });
    n.add("java/util/zip/Deflater.getAdler(J)I", |_, a| {
        ret_i(with_stream(arg_j(a, 0), |s| match s {
            Stream::Deflate { adler, .. } => *adler as i32,
            Stream::Inflate(state, _) => state.decompressor().adler32().unwrap_or(1) as i32,
        }))
    });
    n.add("java/util/zip/Deflater.reset(J)V", |_, a| {
        with_stream(arg_j(a, 0), |s| {
            if let Stream::Deflate { compressor, adler, .. } = s {
                compressor.reset();
                *adler = 1;
            }
        });
        ret_void()
    });
    n.add("java/util/zip/Deflater.end(J)V", end);

    n.add("java/util/zip/CRC32.update(II)I", |_, a| ret_i(crc32(arg_i(a, 0) as u32, &[a[1] as u8]) as i32));
    n.add("java/util/zip/CRC32.updateBytes0(I[BII)I", |t, a| {
        let data = bytes(t.vm, arg_ref(a, 1), arg_i(a, 2), arg_i(a, 3));
        ret_i(crc32(arg_i(a, 0) as u32, data) as i32)
    });
    n.add("java/util/zip/CRC32.updateByteBuffer0(IJII)I", |_, a| {
        let data = native_bytes(arg_j(a, 1) + arg_i(a, 3) as i64, arg_i(a, 4));
        ret_i(crc32(arg_i(a, 0) as u32, data) as i32)
    });
    n.add("java/util/zip/Adler32.update(II)I", |_, a| ret_i(adler32(arg_i(a, 0) as u32, &[a[1] as u8]) as i32));
    n.add("java/util/zip/Adler32.updateBytes(I[BII)I", |t, a| {
        let data = bytes(t.vm, arg_ref(a, 1), arg_i(a, 2), arg_i(a, 3));
        ret_i(adler32(arg_i(a, 0) as u32, data) as i32)
    });
    n.add("java/util/zip/Adler32.updateByteBuffer(IJII)I", |_, a| {
        let data = native_bytes(arg_j(a, 1) + arg_i(a, 3) as i64, arg_i(a, 4));
        ret_i(adler32(arg_i(a, 0) as u32, data) as i32)
    });
}

fn end(_: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    if let Some(map) = STREAMS.lock().unwrap().1.as_mut() {
        map.remove(&arg_j(a, 0));
    }
    ret_void()
}

fn inflate(t: &mut Thread<'_>, handle: i64, input: &[u8], output: &mut [u8]) -> JResult<Option<u64>> {
    let result = with_stream(handle, |s| match s {
        Stream::Inflate(state, _) => miniz_oxide::inflate::stream::inflate(state, input, output, MZFlush::None),
        Stream::Deflate { .. } => unreachable!("deflate stream used for inflate"),
    });
    match result.status {
        Ok(status) => ret_j(pack(result.bytes_consumed, result.bytes_written, status == MZStatus::StreamEnd, status == MZStatus::NeedDict)),
        Err(MZError::Buf) => ret_j(pack(result.bytes_consumed, result.bytes_written, false, false)),
        Err(error) => {
            let message = match error {
                MZError::Data => "invalid stored block lengths or corrupt data",
                _ => "inflate error",
            };
            Err(t.throw_new("java/util/zip/DataFormatException", Some(message)))
        }
    }
}

/// Deflater strategy constants: DEFAULT_STRATEGY 0, FILTERED 1, HUFFMAN_ONLY 2.
fn configure(compressor: &mut CompressorOxide, level: i32, strategy: i32, raw: bool) {
    let level = if level < 0 { 6 } else { level.min(10) };
    let window_bits = if raw { -15 } else { 15 };
    compressor.set_compression_level_raw(level as u8);
    let flags = create_comp_flags_from_zip_params(level, window_bits, strategy);
    compressor.set_format_and_level(if raw { DataFormat::Raw } else { DataFormat::Zlib }, level as u8);
    let _ = flags;
}

fn deflate(handle: i64, input: &[u8], output: &mut [u8], flush: i32, params: i32) -> JResult<Option<u64>> {
    with_stream(handle, |s| {
        let Stream::Deflate { compressor, raw, adler } = s else { unreachable!("inflate stream used for deflate") };
        // params: bit 0 set → apply; bits 1–2 strategy; bits 3.. level.
        let params_applied = params & 1 != 0;
        if params_applied {
            configure(compressor, params >> 3, (params >> 1) & 3, *raw);
        }
        let flush = match flush {
            2 => MZFlush::Sync,
            3 => MZFlush::Full,
            4 => MZFlush::Finish,
            _ => MZFlush::None,
        };
        let result = miniz_oxide::deflate::stream::deflate(compressor, input, output, flush);
        *adler = compressor.adler32();
        let finished = matches!(result.status, Ok(MZStatus::StreamEnd));
        ret_j(pack(result.bytes_consumed, result.bytes_written, finished, params_applied))
    })
}

fn crc32(crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, entry) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *entry = c;
        }
        table
    });
    let mut c = !crc;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

fn adler32(adler: u32, data: &[u8]) -> u32 {
    let (mut a, mut b) = (adler & 0xFFFF, adler >> 16);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}
