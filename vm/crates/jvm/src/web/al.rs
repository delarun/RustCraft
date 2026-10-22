//! OpenAL 1.1 for LWJGL, mixed in software: sources play mono or stereo
//! buffers (static or queued), with pitch, gain, distance attenuation and
//! stereo panning of mono sources around the listener. A mixer thread fills
//! a ring of 48 kHz stereo samples in the VM's memory, which the page's
//! AudioWorklet plays (launcher/src/jvm/audio.ts).

use std::collections::HashMap;
use std::ffi::c_char;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::c_str;

#[cfg_attr(not(rjava_stub), link(wasm_import_module = "rjava_web"))]
unsafe extern "C" {
    /// Starts playback of the ring at `ring` (see `Ring`).
    fn audio_start(ring: u32);
}

pub const SAMPLE_RATE: u32 = 48_000;
/// Ring capacity in frames, and how far ahead the mixer fills it.
const RING_FRAMES: usize = 16_384;
const AHEAD_FRAMES: u32 = 4_096;
const CHUNK_FRAMES: usize = 256;

/// The output ring: written by the mixer, read by the AudioWorklet.
#[repr(C)]
struct Ring {
    /// Frames written and read, ever (wrapping).
    written: AtomicU32,
    read: AtomicU32,
    capacity: u32,
    rate: u32,
    samples: [f32; RING_FRAMES * 2],
}

// AL enums.
const AL_NO_ERROR: i32 = 0;
const AL_INVALID_NAME: i32 = 0xA001;
const AL_INVALID_ENUM: i32 = 0xA002;
const AL_INVALID_VALUE: i32 = 0xA003;
const AL_INVALID_OPERATION: i32 = 0xA004;
const AL_SOURCE_RELATIVE: i32 = 0x202;
const AL_CONE_INNER_ANGLE: i32 = 0x1001;
const AL_CONE_OUTER_ANGLE: i32 = 0x1002;
const AL_PITCH: i32 = 0x1003;
const AL_POSITION: i32 = 0x1004;
const AL_DIRECTION: i32 = 0x1005;
const AL_VELOCITY: i32 = 0x1006;
const AL_LOOPING: i32 = 0x1007;
const AL_BUFFER: i32 = 0x1009;
const AL_GAIN: i32 = 0x100A;
const AL_MIN_GAIN: i32 = 0x100D;
const AL_MAX_GAIN: i32 = 0x100E;
const AL_ORIENTATION: i32 = 0x100F;
const AL_SOURCE_STATE: i32 = 0x1010;
const AL_INITIAL: i32 = 0x1011;
const AL_PLAYING: i32 = 0x1012;
const AL_PAUSED: i32 = 0x1013;
const AL_STOPPED: i32 = 0x1014;
const AL_BUFFERS_QUEUED: i32 = 0x1015;
const AL_BUFFERS_PROCESSED: i32 = 0x1016;
const AL_REFERENCE_DISTANCE: i32 = 0x1020;
const AL_ROLLOFF_FACTOR: i32 = 0x1021;
const AL_CONE_OUTER_GAIN: i32 = 0x1022;
const AL_MAX_DISTANCE: i32 = 0x1023;
const AL_SEC_OFFSET: i32 = 0x1024;
const AL_SAMPLE_OFFSET: i32 = 0x1025;
const AL_BYTE_OFFSET: i32 = 0x1026;
const AL_SOURCE_TYPE: i32 = 0x1027;
const AL_STATIC: i32 = 0x1028;
const AL_STREAMING: i32 = 0x1029;
const AL_UNDETERMINED: i32 = 0x1030;
const AL_FORMAT_MONO8: i32 = 0x1100;
const AL_FORMAT_MONO16: i32 = 0x1101;
const AL_FORMAT_STEREO8: i32 = 0x1102;
const AL_FORMAT_STEREO16: i32 = 0x1103;
const AL_FREQUENCY: i32 = 0x2001;
const AL_BITS: i32 = 0x2002;
const AL_CHANNELS: i32 = 0x2003;
const AL_SIZE: i32 = 0x2004;
const AL_VENDOR: i32 = 0xB001;
const AL_VERSION: i32 = 0xB002;
const AL_RENDERER: i32 = 0xB003;
const AL_EXTENSIONS: i32 = 0xB004;
const AL_DOPPLER_FACTOR: i32 = 0xC000;
const AL_SPEED_OF_SOUND: i32 = 0xC003;
const AL_DISTANCE_MODEL: i32 = 0xD000;
const AL_INVERSE_DISTANCE: i32 = 0xD001;
const AL_INVERSE_DISTANCE_CLAMPED: i32 = 0xD002;
const AL_LINEAR_DISTANCE: i32 = 0xD003;
const AL_LINEAR_DISTANCE_CLAMPED: i32 = 0xD004;
const AL_EXPONENT_DISTANCE: i32 = 0xD005;
const AL_EXPONENT_DISTANCE_CLAMPED: i32 = 0xD006;

// ALC enums.
const ALC_MAJOR_VERSION: i32 = 0x1000;
const ALC_MINOR_VERSION: i32 = 0x1001;
const ALC_ATTRIBUTES_SIZE: i32 = 0x1002;
const ALC_ALL_ATTRIBUTES: i32 = 0x1003;
const ALC_DEFAULT_DEVICE_SPECIFIER: i32 = 0x1004;
const ALC_DEVICE_SPECIFIER: i32 = 0x1005;
const ALC_EXTENSIONS: i32 = 0x1006;
const ALC_FREQUENCY: i32 = 0x1007;
const ALC_REFRESH: i32 = 0x1008;
const ALC_SYNC: i32 = 0x1009;
const ALC_MONO_SOURCES: i32 = 0x1010;
const ALC_STEREO_SOURCES: i32 = 0x1011;
const ALC_DEFAULT_ALL_DEVICES_SPECIFIER: i32 = 0x1012;
const ALC_ALL_DEVICES_SPECIFIER: i32 = 0x1013;

const MAX_SOURCES: i32 = 255;

struct Buffer {
    channels: usize,
    rate: u32,
    bits: i32,
    /// Interleaved samples in [-1, 1].
    samples: Arc<Vec<f32>>,
}

impl Buffer {
    fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }
}

#[derive(Clone)]
struct Source {
    state: i32,
    queue: Vec<u32>,
    /// Index in `queue` of the buffer playing, and the frame position in it.
    current: usize,
    position: f64,
    static_buffer: bool,
    looping: bool,
    relative: bool,
    gain: f32,
    min_gain: f32,
    max_gain: f32,
    pitch: f32,
    location: [f32; 3],
    velocity: [f32; 3],
    direction: [f32; 3],
    reference_distance: f32,
    max_distance: f32,
    rolloff: f32,
    cone: [f32; 3],
    /// AL_EXT_source_distance_model: this source's model (AL_NONE: the context's).
    distance_model: i32,
}

impl Default for Source {
    fn default() -> Source {
        Source {
            state: AL_INITIAL,
            queue: Vec::new(),
            current: 0,
            position: 0.0,
            static_buffer: false,
            looping: false,
            relative: false,
            gain: 1.0,
            min_gain: 0.0,
            max_gain: 1.0,
            pitch: 1.0,
            location: [0.0; 3],
            velocity: [0.0; 3],
            direction: [0.0; 3],
            reference_distance: 1.0,
            max_distance: f32::MAX,
            rolloff: 1.0,
            cone: [360.0, 360.0, 0.0],
            distance_model: 0,
        }
    }
}

struct Al {
    error: i32,
    buffers: HashMap<u32, Buffer>,
    sources: HashMap<u32, Source>,
    next_name: u32,
    listener_position: [f32; 3],
    listener_velocity: [f32; 3],
    listener_orientation: [f32; 6],
    listener_gain: f32,
    distance_model: i32,
    doppler: f32,
    speed_of_sound: f32,
    mixer: bool,
    /// AL_SOURCE_DISTANCE_MODEL enabled: sources' own models apply.
    source_distance_model: bool,
}

static AL: Mutex<Option<Al>> = Mutex::new(None);

fn al() -> MutexGuard<'static, Option<Al>> {
    let mut guard = AL.lock().unwrap();
    guard.get_or_insert_with(|| Al {
        error: AL_NO_ERROR,
        buffers: HashMap::new(),
        sources: HashMap::new(),
        next_name: 1,
        listener_position: [0.0; 3],
        listener_velocity: [0.0; 3],
        listener_orientation: [0.0, 0.0, -1.0, 0.0, 1.0, 0.0],
        listener_gain: 1.0,
        distance_model: AL_INVERSE_DISTANCE_CLAMPED,
        doppler: 1.0,
        speed_of_sound: 343.3,
        mixer: false,
        source_distance_model: false,
    });
    guard
}

macro_rules! with_al {
    (|$a:ident| $body:expr) => {{
        let mut guard = al();
        let $a = guard.as_mut().unwrap();
        $body
    }};
}

impl Al {
    fn fail(&mut self, error: i32) {
        if self.error == AL_NO_ERROR {
            self.error = error;
        }
    }

    fn source(&mut self, name: u32) -> Option<&mut Source> {
        if !self.sources.contains_key(&name) {
            self.fail(AL_INVALID_NAME);
        }
        self.sources.get_mut(&name)
    }

    fn processed(&self, source: &Source) -> usize {
        if source.state == AL_STOPPED || source.state == AL_INITIAL && source.current > 0 {
            if source.state == AL_STOPPED { source.queue.len() } else { source.current }
        } else {
            source.current.min(source.queue.len())
        }
    }
}

fn read<T: Copy>(p: u32) -> T {
    // SAFETY: the caller's pointer.
    unsafe { (p as usize as *const T).read_unaligned() }
}

fn write<T>(p: u32, v: T) {
    if p != 0 {
        // SAFETY: the caller's pointer.
        unsafe { (p as usize as *mut T).write_unaligned(v) }
    }
}

fn floats<const N: usize>(p: u32) -> [f32; N] {
    std::array::from_fn(|i| read::<f32>(p + i as u32 * 4))
}

fn static_c_str(s: &'static [u8]) -> *const c_char {
    s.as_ptr() as *const c_char
}

// --- ALC: devices and contexts ----------------------------------------------

const DEVICE: u32 = 0xA1C0;
const CONTEXT: u32 = 0xA1C1;

extern "C" fn alc_open_device(_name: *const c_char) -> u32 {
    start_mixer();
    DEVICE
}
extern "C" fn alc_close_device(_device: u32) -> bool {
    true
}
extern "C" fn alc_create_context(_device: u32, _attributes: u32) -> u32 {
    CONTEXT
}
extern "C" fn alc_make_context_current(_context: u32) -> bool {
    true
}
extern "C" fn alc_context_noop(_context: u32) {}
extern "C" fn alc_get_current_context() -> u32 {
    CONTEXT
}
extern "C" fn alc_get_contexts_device(_context: u32) -> u32 {
    DEVICE
}
extern "C" fn alc_get_error(_device: u32) -> i32 {
    0
}
extern "C" fn alc_is_extension_present(_device: u32, _name: *const c_char) -> bool {
    false
}
extern "C" fn alc_get_proc_address(_device: u32, name: *const c_char) -> usize {
    symbol(c_str(name as u32)).unwrap_or(0)
}
extern "C" fn alc_get_enum_value(_device: u32, _name: *const c_char) -> i32 {
    0
}
extern "C" fn alc_get_string(_device: u32, param: i32) -> *const c_char {
    match param {
        ALC_DEFAULT_DEVICE_SPECIFIER | ALC_DEVICE_SPECIFIER | ALC_DEFAULT_ALL_DEVICES_SPECIFIER => static_c_str(b"Web Audio\0"),
        // Lists end with an empty string.
        ALC_ALL_DEVICES_SPECIFIER => static_c_str(b"Web Audio\0\0"),
        ALC_EXTENSIONS => static_c_str(b"\0"),
        0 => static_c_str(b"No Error\0"),
        _ => static_c_str(b"\0"),
    }
}
extern "C" fn alc_get_integerv(_device: u32, param: i32, size: i32, values: u32) {
    if size < 1 || values == 0 {
        return;
    }
    let attributes = [ALC_FREQUENCY, SAMPLE_RATE as i32, ALC_REFRESH, 50, ALC_SYNC, 0, ALC_MONO_SOURCES, MAX_SOURCES, ALC_STEREO_SOURCES, MAX_SOURCES, 0];
    match param {
        ALC_MAJOR_VERSION => write(values, 1i32),
        ALC_MINOR_VERSION => write(values, 1i32),
        ALC_FREQUENCY => write(values, SAMPLE_RATE as i32),
        ALC_REFRESH => write(values, 50i32),
        ALC_SYNC => write(values, 0i32),
        ALC_MONO_SOURCES | ALC_STEREO_SOURCES => write(values, MAX_SOURCES),
        ALC_ATTRIBUTES_SIZE => write(values, attributes.len() as i32),
        ALC_ALL_ATTRIBUTES => {
            for (i, &v) in attributes.iter().take(size as usize).enumerate() {
                write(values + i as u32 * 4, v);
            }
        }
        _ => write(values, 0i32),
    }
}

// --- AL: state ---------------------------------------------------------------

const AL_SOURCE_DISTANCE_MODEL: i32 = 0x200;

extern "C" fn al_enable(capability: i32) {
    if capability == AL_SOURCE_DISTANCE_MODEL {
        with_al!(|a| a.source_distance_model = true)
    }
}
extern "C" fn al_disable(capability: i32) {
    if capability == AL_SOURCE_DISTANCE_MODEL {
        with_al!(|a| a.source_distance_model = false)
    }
}
extern "C" fn al_is_enabled(capability: i32) -> bool {
    capability == AL_SOURCE_DISTANCE_MODEL && with_al!(|a| a.source_distance_model)
}

/// The AL extensions provided.
const EXTENSIONS: &[&str] = &["AL_EXT_source_distance_model"];
extern "C" fn al_get_error() -> i32 {
    with_al!(|a| std::mem::replace(&mut a.error, AL_NO_ERROR))
}
extern "C" fn al_is_extension_present(name: *const c_char) -> bool {
    let name = c_str(name as u32);
    EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(name))
}
extern "C" fn al_get_proc_address(name: *const c_char) -> usize {
    symbol(c_str(name as u32)).unwrap_or(0)
}
extern "C" fn al_get_enum_value(_name: *const c_char) -> i32 {
    0
}
extern "C" fn al_get_string(param: i32) -> *const c_char {
    match param {
        AL_VENDOR => static_c_str(b"rjava\0"),
        AL_VERSION => static_c_str(b"1.1 Web Audio\0"),
        AL_RENDERER => static_c_str(b"Software mixer\0"),
        AL_EXTENSIONS => static_c_str(b"AL_EXT_source_distance_model\0"),
        AL_NO_ERROR => static_c_str(b"No Error\0"),
        AL_INVALID_NAME => static_c_str(b"Invalid Name\0"),
        AL_INVALID_ENUM => static_c_str(b"Invalid Enum\0"),
        AL_INVALID_VALUE => static_c_str(b"Invalid Value\0"),
        AL_INVALID_OPERATION => static_c_str(b"Invalid Operation\0"),
        _ => static_c_str(b"\0"),
    }
}
extern "C" fn al_distance_model(model: i32) {
    with_al!(|a| a.distance_model = model)
}
extern "C" fn al_doppler_factor(v: f32) {
    with_al!(|a| a.doppler = v)
}
extern "C" fn al_speed_of_sound(v: f32) {
    with_al!(|a| a.speed_of_sound = v)
}
extern "C" fn al_doppler_velocity(_v: f32) {}
extern "C" fn al_get_integer(param: i32) -> i32 {
    with_al!(|a| match param {
        AL_DISTANCE_MODEL => a.distance_model,
        _ => 0,
    })
}
extern "C" fn al_get_float(param: i32) -> f32 {
    with_al!(|a| match param {
        AL_DOPPLER_FACTOR => a.doppler,
        AL_SPEED_OF_SOUND => a.speed_of_sound,
        _ => 0.0,
    })
}
extern "C" fn al_get_integerv(param: i32, values: u32) {
    write(values, al_get_integer(param));
}
extern "C" fn al_get_floatv(param: i32, values: u32) {
    write(values, al_get_float(param));
}

// --- listener ------------------------------------------------------------------

extern "C" fn al_listenerf(param: i32, v: f32) {
    with_al!(|a| match param {
        AL_GAIN => a.listener_gain = v,
        _ => a.fail(AL_INVALID_ENUM),
    })
}
extern "C" fn al_listener3f(param: i32, x: f32, y: f32, z: f32) {
    with_al!(|a| match param {
        AL_POSITION => a.listener_position = [x, y, z],
        AL_VELOCITY => a.listener_velocity = [x, y, z],
        _ => a.fail(AL_INVALID_ENUM),
    })
}
extern "C" fn al_listenerfv(param: i32, values: u32) {
    with_al!(|a| match param {
        AL_GAIN => a.listener_gain = read(values),
        AL_POSITION => a.listener_position = floats(values),
        AL_VELOCITY => a.listener_velocity = floats(values),
        AL_ORIENTATION => a.listener_orientation = floats(values),
        _ => a.fail(AL_INVALID_ENUM),
    })
}
extern "C" fn al_listeneri(_param: i32, _v: i32) {}
extern "C" fn al_get_listenerf(param: i32, value: u32) {
    with_al!(|a| write(value, if param == AL_GAIN { a.listener_gain } else { 0.0 }))
}
extern "C" fn al_get_listener3f(param: i32, x: u32, y: u32, z: u32) {
    let v = with_al!(|a| if param == AL_VELOCITY { a.listener_velocity } else { a.listener_position });
    write(x, v[0]);
    write(y, v[1]);
    write(z, v[2]);
}
extern "C" fn al_get_listenerfv(param: i32, values: u32) {
    with_al!(|a| {
        let v: Vec<f32> = match param {
            AL_GAIN => vec![a.listener_gain],
            AL_POSITION => a.listener_position.to_vec(),
            AL_VELOCITY => a.listener_velocity.to_vec(),
            AL_ORIENTATION => a.listener_orientation.to_vec(),
            _ => vec![0.0],
        };
        for (i, f) in v.into_iter().enumerate() {
            write(values + i as u32 * 4, f);
        }
    })
}

// --- buffers -------------------------------------------------------------------

extern "C" fn al_gen_buffers(n: i32, names: u32) {
    with_al!(|a| {
        for i in 0..n.max(0) as u32 {
            let name = a.next_name;
            a.next_name += 1;
            a.buffers.insert(name, Buffer { channels: 1, rate: SAMPLE_RATE, bits: 16, samples: Arc::new(Vec::new()) });
            write(names + i * 4, name);
        }
    })
}
extern "C" fn al_delete_buffers(n: i32, names: u32) {
    with_al!(|a| {
        for i in 0..n.max(0) as u32 {
            let name: u32 = read(names + i * 4);
            if name != 0 {
                a.buffers.remove(&name);
            }
        }
    })
}
extern "C" fn al_is_buffer(name: u32) -> bool {
    with_al!(|a| name == 0 || a.buffers.contains_key(&name))
}
extern "C" fn al_buffer_data(name: u32, format: i32, data: u32, size: i32, rate: i32) {
    let (channels, bits) = match format {
        AL_FORMAT_MONO8 => (1, 8),
        AL_FORMAT_MONO16 => (1, 16),
        AL_FORMAT_STEREO8 => (2, 8),
        AL_FORMAT_STEREO16 => (2, 16),
        _ => return with_al!(|a| a.fail(AL_INVALID_ENUM)),
    };
    let size = size.max(0) as usize;
    // SAFETY: the caller's data of `size` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data as usize as *const u8, size) };
    let samples: Vec<f32> = if bits == 8 {
        bytes.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect()
    } else {
        bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect()
    };
    with_al!(|a| match a.buffers.get_mut(&name) {
        Some(buffer) => *buffer = Buffer { channels, rate: rate.max(1) as u32, bits, samples: Arc::new(samples) },
        None => a.fail(AL_INVALID_NAME),
    })
}
extern "C" fn al_get_bufferi(name: u32, param: i32, value: u32) {
    with_al!(|a| match a.buffers.get(&name) {
        Some(b) => write(
            value,
            match param {
                AL_FREQUENCY => b.rate as i32,
                AL_BITS => b.bits,
                AL_CHANNELS => b.channels as i32,
                AL_SIZE => (b.samples.len() * b.bits as usize / 8) as i32,
                _ => 0,
            },
        ),
        None => a.fail(AL_INVALID_NAME),
    })
}
extern "C" fn al_bufferi(_name: u32, _param: i32, _value: i32) {}

// --- sources -------------------------------------------------------------------

extern "C" fn al_gen_sources(n: i32, names: u32) {
    with_al!(|a| {
        for i in 0..n.max(0) as u32 {
            let name = a.next_name;
            a.next_name += 1;
            a.sources.insert(name, Source::default());
            write(names + i * 4, name);
        }
    })
}
extern "C" fn al_delete_sources(n: i32, names: u32) {
    with_al!(|a| {
        for i in 0..n.max(0) as u32 {
            a.sources.remove(&read::<u32>(names + i * 4));
        }
    })
}
extern "C" fn al_is_source(name: u32) -> bool {
    with_al!(|a| a.sources.contains_key(&name))
}
extern "C" fn al_sourcef(name: u32, param: i32, v: f32) {
    with_al!(|a| {
        let rate = a.sources.get(&name).and_then(|s| s.queue.get(s.current)).and_then(|b| a.buffers.get(b)).map_or(SAMPLE_RATE, |b| b.rate);
        let Some(s) = a.source(name) else { return };
        match param {
            AL_PITCH => s.pitch = v.max(0.0),
            AL_GAIN => s.gain = v.max(0.0),
            AL_MIN_GAIN => s.min_gain = v,
            AL_MAX_GAIN => s.max_gain = v,
            AL_REFERENCE_DISTANCE => s.reference_distance = v,
            AL_ROLLOFF_FACTOR => s.rolloff = v,
            AL_MAX_DISTANCE => s.max_distance = v,
            AL_CONE_INNER_ANGLE => s.cone[0] = v,
            AL_CONE_OUTER_ANGLE => s.cone[1] = v,
            AL_CONE_OUTER_GAIN => s.cone[2] = v,
            AL_SEC_OFFSET => s.position = v as f64 * rate as f64,
            AL_SAMPLE_OFFSET => s.position = v as f64,
            _ => a.fail(AL_INVALID_ENUM),
        }
    })
}
extern "C" fn al_source3f(name: u32, param: i32, x: f32, y: f32, z: f32) {
    with_al!(|a| {
        let Some(s) = a.source(name) else { return };
        match param {
            AL_POSITION => s.location = [x, y, z],
            AL_VELOCITY => s.velocity = [x, y, z],
            AL_DIRECTION => s.direction = [x, y, z],
            _ => a.fail(AL_INVALID_ENUM),
        }
    })
}
extern "C" fn al_sourcefv(name: u32, param: i32, values: u32) {
    match param {
        AL_POSITION | AL_VELOCITY | AL_DIRECTION => {
            let [x, y, z] = floats::<3>(values);
            al_source3f(name, param, x, y, z);
        }
        _ => al_sourcef(name, param, read(values)),
    }
}
extern "C" fn al_sourcei(name: u32, param: i32, v: i32) {
    with_al!(|a| {
        if param == AL_BUFFER && v != 0 && !a.buffers.contains_key(&(v as u32)) {
            return a.fail(AL_INVALID_VALUE);
        }
        let Some(s) = a.source(name) else { return };
        match param {
            AL_BUFFER => {
                s.queue = if v == 0 { Vec::new() } else { vec![v as u32] };
                s.static_buffer = v != 0;
                s.current = 0;
                s.position = 0.0;
            }
            AL_LOOPING => s.looping = v != 0,
            AL_SOURCE_RELATIVE => s.relative = v != 0,
            AL_SAMPLE_OFFSET => s.position = v as f64,
            AL_MAX_DISTANCE => s.max_distance = v as f32,
            AL_REFERENCE_DISTANCE => s.reference_distance = v as f32,
            AL_ROLLOFF_FACTOR => s.rolloff = v as f32,
            AL_CONE_INNER_ANGLE => s.cone[0] = v as f32,
            AL_CONE_OUTER_ANGLE => s.cone[1] = v as f32,
            AL_DISTANCE_MODEL => s.distance_model = v,
            _ => a.fail(AL_INVALID_ENUM),
        }
    })
}
extern "C" fn al_source3i(name: u32, param: i32, x: i32, y: i32, z: i32) {
    al_source3f(name, param, x as f32, y as f32, z as f32);
}
extern "C" fn al_sourceiv(name: u32, param: i32, values: u32) {
    al_sourcei(name, param, read(values));
}
extern "C" fn al_get_sourcei(name: u32, param: i32, value: u32) {
    with_al!(|a| {
        let Some(s) = a.sources.get(&name).cloned() else { return a.fail(AL_INVALID_NAME) };
        let v = match param {
            AL_SOURCE_STATE => s.state,
            AL_BUFFERS_QUEUED => s.queue.len() as i32,
            AL_BUFFERS_PROCESSED => {
                if s.static_buffer { 0 } else { a.processed(&s) as i32 }
            }
            AL_BUFFER => s.queue.get(s.current).copied().unwrap_or(0) as i32,
            AL_LOOPING => s.looping as i32,
            AL_SOURCE_RELATIVE => s.relative as i32,
            AL_SOURCE_TYPE => {
                if s.queue.is_empty() { AL_UNDETERMINED } else if s.static_buffer { AL_STATIC } else { AL_STREAMING }
            }
            AL_SAMPLE_OFFSET => s.position as i32,
            AL_BYTE_OFFSET => s.position as i32 * 2,
            _ => 0,
        };
        write(value, v);
    })
}
extern "C" fn al_get_sourceiv(name: u32, param: i32, values: u32) {
    al_get_sourcei(name, param, values);
}
extern "C" fn al_get_sourcef(name: u32, param: i32, value: u32) {
    with_al!(|a| {
        let Some(s) = a.sources.get(&name).cloned() else { return a.fail(AL_INVALID_NAME) };
        let v = match param {
            AL_PITCH => s.pitch,
            AL_GAIN => s.gain,
            AL_MIN_GAIN => s.min_gain,
            AL_MAX_GAIN => s.max_gain,
            AL_REFERENCE_DISTANCE => s.reference_distance,
            AL_ROLLOFF_FACTOR => s.rolloff,
            AL_MAX_DISTANCE => s.max_distance,
            AL_SEC_OFFSET => {
                let rate = s.queue.get(s.current).and_then(|b| a.buffers.get(b)).map_or(SAMPLE_RATE, |b| b.rate);
                (s.position / rate as f64) as f32
            }
            AL_SAMPLE_OFFSET => s.position as f32,
            _ => 0.0,
        };
        write(value, v);
    })
}
extern "C" fn al_get_source3f(name: u32, param: i32, x: u32, y: u32, z: u32) {
    let v = with_al!(|a| a.sources.get(&name).map(|s| match param {
        AL_VELOCITY => s.velocity,
        AL_DIRECTION => s.direction,
        _ => s.location,
    }));
    let v = v.unwrap_or([0.0; 3]);
    write(x, v[0]);
    write(y, v[1]);
    write(z, v[2]);
}
extern "C" fn al_get_sourcefv(name: u32, param: i32, values: u32) {
    match param {
        AL_POSITION | AL_VELOCITY | AL_DIRECTION => al_get_source3f(name, param, values, values + 4, values + 8),
        _ => al_get_sourcef(name, param, values),
    }
}

fn set_state(name: u32, state: i32) {
    with_al!(|a| {
        let Some(s) = a.source(name) else { return };
        match state {
            AL_PLAYING => {
                if s.state != AL_PAUSED {
                    s.current = 0;
                    s.position = 0.0;
                }
                s.state = if s.queue.is_empty() { AL_STOPPED } else { AL_PLAYING };
            }
            AL_PAUSED => {
                if s.state == AL_PLAYING {
                    s.state = AL_PAUSED;
                }
            }
            AL_STOPPED => {
                if s.state != AL_INITIAL {
                    s.state = AL_STOPPED;
                }
            }
            _ => {
                s.state = AL_INITIAL;
                s.current = 0;
                s.position = 0.0;
            }
        }
    })
}
extern "C" fn al_source_play(name: u32) {
    set_state(name, AL_PLAYING);
}
extern "C" fn al_source_pause(name: u32) {
    set_state(name, AL_PAUSED);
}
extern "C" fn al_source_stop(name: u32) {
    set_state(name, AL_STOPPED);
}
extern "C" fn al_source_rewind(name: u32) {
    set_state(name, AL_INITIAL);
}
macro_rules! vector_op {
    ($name:ident, $op:ident) => {
        extern "C" fn $name(n: i32, names: u32) {
            for i in 0..n.max(0) as u32 {
                $op(read(names + i * 4));
            }
        }
    };
}
vector_op!(al_source_playv, al_source_play);
vector_op!(al_source_pausev, al_source_pause);
vector_op!(al_source_stopv, al_source_stop);
vector_op!(al_source_rewindv, al_source_rewind);

extern "C" fn al_source_queue_buffers(name: u32, n: i32, buffers: u32) {
    with_al!(|a| {
        let names: Vec<u32> = (0..n.max(0) as u32).map(|i| read(buffers + i * 4)).collect();
        if names.iter().any(|b| *b != 0 && !a.buffers.contains_key(b)) {
            return a.fail(AL_INVALID_NAME);
        }
        let Some(s) = a.source(name) else { return };
        if s.static_buffer {
            s.queue.clear();
            s.static_buffer = false;
        }
        s.queue.extend(names);
    })
}
extern "C" fn al_source_unqueue_buffers(name: u32, n: i32, buffers: u32) {
    with_al!(|a| {
        let Some(s) = a.sources.get(&name).cloned() else { return a.fail(AL_INVALID_NAME) };
        let processed = a.processed(&s);
        let n = n.max(0) as usize;
        if n > processed || s.static_buffer {
            return a.fail(AL_INVALID_VALUE);
        }
        let s = a.sources.get_mut(&name).unwrap();
        for (i, b) in s.queue.drain(..n).enumerate() {
            write(buffers + i as u32 * 4, b);
        }
        s.current = s.current.saturating_sub(n);
    })
}

// --- mixing --------------------------------------------------------------------

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn normalize(a: [f32; 3]) -> [f32; 3] {
    let length = dot(a, a).sqrt();
    if length > 0.0 { [a[0] / length, a[1] / length, a[2] / length] } else { a }
}

/// A source's left and right gains for this chunk.
fn source_gains(a: &Al, s: &Source, channels: usize) -> (f32, f32) {
    let mut gain = s.gain * a.listener_gain;
    if channels == 2 {
        let g = gain.clamp(s.min_gain, s.max_gain);
        return (g, g);
    }
    let at = normalize([a.listener_orientation[0], a.listener_orientation[1], a.listener_orientation[2]]);
    let up = normalize([a.listener_orientation[3], a.listener_orientation[4], a.listener_orientation[5]]);
    let right = normalize(cross(at, up));
    let relative = if s.relative { s.location } else { sub(s.location, a.listener_position) };
    let local = if s.relative { relative } else { [dot(relative, right), dot(relative, up), dot(relative, at)] };
    let distance = dot(local, local).sqrt();
    let (reference, max, rolloff) = (s.reference_distance, s.max_distance, s.rolloff);
    let clamped = |d: f32| d.max(reference).min(max);
    let model = if a.source_distance_model && s.distance_model != 0 { s.distance_model } else { a.distance_model };
    let attenuation = match model {
        AL_INVERSE_DISTANCE | AL_INVERSE_DISTANCE_CLAMPED => {
            let d = if model == AL_INVERSE_DISTANCE_CLAMPED { clamped(distance) } else { distance };
            reference / (reference + rolloff * (d - reference)).max(f32::MIN_POSITIVE)
        }
        AL_LINEAR_DISTANCE | AL_LINEAR_DISTANCE_CLAMPED => {
            let d = if model == AL_LINEAR_DISTANCE_CLAMPED { clamped(distance) } else { distance.min(max) };
            if max > reference { 1.0 - rolloff * (d - reference) / (max - reference) } else { 1.0 }
        }
        AL_EXPONENT_DISTANCE | AL_EXPONENT_DISTANCE_CLAMPED => {
            let d = if model == AL_EXPONENT_DISTANCE_CLAMPED { clamped(distance) } else { distance };
            if reference > 0.0 && d > 0.0 { (d / reference).powf(-rolloff) } else { 1.0 }
        }
        _ => 1.0,
    };
    gain = (gain * attenuation.clamp(0.0, 1.0)).clamp(s.min_gain, s.max_gain);
    // Equal-power panning by the direction's left-right component.
    let pan = if distance > 1e-4 { (local[0] / distance).clamp(-1.0, 1.0) } else { 0.0 };
    let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
    (gain * angle.cos(), gain * angle.sin())
}

/// Mixes `out.len() / 2` frames of all playing sources into `out`.
fn mix(a: &mut Al, out: &mut [f32]) {
    out.fill(0.0);
    let frames = out.len() / 2;
    let names: Vec<u32> = a.sources.iter().filter(|(_, s)| s.state == AL_PLAYING).map(|(&n, _)| n).collect();
    for name in names {
        let mut s = a.sources[&name].clone();
        let Some(first) = s.queue.get(s.current).and_then(|b| a.buffers.get(b)) else {
            s.state = AL_STOPPED;
            a.sources.insert(name, s);
            continue;
        };
        let (left, right) = source_gains(a, &s, first.channels);
        let mut frame = 0;
        while frame < frames && s.state == AL_PLAYING {
            let Some(buffer) = s.queue.get(s.current).and_then(|b| a.buffers.get(b)) else {
                s.state = AL_STOPPED;
                break;
            };
            let step = s.pitch as f64 * buffer.rate as f64 / SAMPLE_RATE as f64;
            let length = buffer.frames();
            let samples = &buffer.samples;
            let channels = buffer.channels;
            while frame < frames && (s.position as usize) < length {
                let i = s.position as usize;
                let t = (s.position - i as f64) as f32;
                let next = (i + 1).min(length - 1);
                let (l, r) = if channels == 2 {
                    let l = samples[i * 2] + (samples[next * 2] - samples[i * 2]) * t;
                    let r = samples[i * 2 + 1] + (samples[next * 2 + 1] - samples[i * 2 + 1]) * t;
                    (l, r)
                } else {
                    let m = samples[i] + (samples[next] - samples[i]) * t;
                    (m, m)
                };
                out[frame * 2] += l * left;
                out[frame * 2 + 1] += r * right;
                s.position += step;
                frame += 1;
            }
            if s.position as usize >= length {
                s.position -= length as f64;
                if s.current + 1 < s.queue.len() {
                    s.current += 1;
                } else if s.looping {
                    s.current = 0;
                } else {
                    s.current = s.queue.len();
                    s.state = AL_STOPPED;
                }
            }
            if length == 0 {
                break;
            }
        }
        a.sources.insert(name, s);
    }
    for sample in out.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
}

fn start_mixer() {
    let start = with_al!(|a| !std::mem::replace(&mut a.mixer, true));
    if !start {
        return;
    }
    let ring: &'static mut Ring = Box::leak(Box::new(Ring {
        written: AtomicU32::new(0),
        read: AtomicU32::new(0),
        capacity: RING_FRAMES as u32,
        rate: SAMPLE_RATE,
        samples: [0.0; RING_FRAMES * 2],
    }));
    let address = ring as *mut Ring as u32;
    // SAFETY: the ring lives forever.
    unsafe { audio_start(address) };
    std::thread::Builder::new()
        .name("OpenAL mixer".into())
        .spawn(move || {
            // SAFETY: the leaked ring; only this thread writes samples and `written`.
            let ring = unsafe { &mut *(address as usize as *mut Ring) };
            let mut chunk = vec![0.0f32; CHUNK_FRAMES * 2];
            loop {
                let written = ring.written.load(Ordering::Relaxed);
                let read = ring.read.load(Ordering::Acquire);
                if written.wrapping_sub(read) >= AHEAD_FRAMES {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                with_al!(|a| mix(a, &mut chunk));
                for (i, frame) in chunk.chunks_exact(2).enumerate() {
                    let at = (written as usize + i) % RING_FRAMES;
                    ring.samples[at * 2] = frame[0];
                    ring.samples[at * 2 + 1] = frame[1];
                }
                ring.written.store(written.wrapping_add(CHUNK_FRAMES as u32), Ordering::Release);
            }
        })
        .expect("starting the OpenAL mixer");
}

/// libopenal's functions by name (also for alGetProcAddress).
pub(super) fn symbol(name: &str) -> Option<usize> {
    macro_rules! table {
        ($($name:literal => $f:expr,)*) => {
            match name {
                $($name => Some($f as *const () as usize),)*
                _ => None,
            }
        };
    }
    table! {
        "alcOpenDevice" => alc_open_device,
        "alcCloseDevice" => alc_close_device,
        "alcCreateContext" => alc_create_context,
        "alcMakeContextCurrent" => alc_make_context_current,
        "alcProcessContext" => alc_context_noop,
        "alcSuspendContext" => alc_context_noop,
        "alcDestroyContext" => alc_context_noop,
        "alcGetCurrentContext" => alc_get_current_context,
        "alcGetContextsDevice" => alc_get_contexts_device,
        "alcGetError" => alc_get_error,
        "alcIsExtensionPresent" => alc_is_extension_present,
        "alcGetProcAddress" => alc_get_proc_address,
        "alcGetEnumValue" => alc_get_enum_value,
        "alcGetString" => alc_get_string,
        "alcGetIntegerv" => alc_get_integerv,
        "alEnable" => al_enable,
        "alDisable" => al_disable,
        "alIsEnabled" => al_is_enabled,
        "alGetError" => al_get_error,
        "alIsExtensionPresent" => al_is_extension_present,
        "alGetProcAddress" => al_get_proc_address,
        "alGetEnumValue" => al_get_enum_value,
        "alGetString" => al_get_string,
        "alDistanceModel" => al_distance_model,
        "alDopplerFactor" => al_doppler_factor,
        "alDopplerVelocity" => al_doppler_velocity,
        "alSpeedOfSound" => al_speed_of_sound,
        "alGetInteger" => al_get_integer,
        "alGetFloat" => al_get_float,
        "alGetIntegerv" => al_get_integerv,
        "alGetFloatv" => al_get_floatv,
        "alListenerf" => al_listenerf,
        "alListener3f" => al_listener3f,
        "alListenerfv" => al_listenerfv,
        "alListeneri" => al_listeneri,
        "alGetListenerf" => al_get_listenerf,
        "alGetListener3f" => al_get_listener3f,
        "alGetListenerfv" => al_get_listenerfv,
        "alGenBuffers" => al_gen_buffers,
        "alDeleteBuffers" => al_delete_buffers,
        "alIsBuffer" => al_is_buffer,
        "alBufferData" => al_buffer_data,
        "alBufferi" => al_bufferi,
        "alGetBufferi" => al_get_bufferi,
        "alGenSources" => al_gen_sources,
        "alDeleteSources" => al_delete_sources,
        "alIsSource" => al_is_source,
        "alSourcef" => al_sourcef,
        "alSource3f" => al_source3f,
        "alSourcefv" => al_sourcefv,
        "alSourcei" => al_sourcei,
        "alSource3i" => al_source3i,
        "alSourceiv" => al_sourceiv,
        "alGetSourcef" => al_get_sourcef,
        "alGetSource3f" => al_get_source3f,
        "alGetSourcefv" => al_get_sourcefv,
        "alGetSourcei" => al_get_sourcei,
        "alGetSourceiv" => al_get_sourceiv,
        "alSourcePlay" => al_source_play,
        "alSourcePause" => al_source_pause,
        "alSourceStop" => al_source_stop,
        "alSourceRewind" => al_source_rewind,
        "alSourcePlayv" => al_source_playv,
        "alSourcePausev" => al_source_pausev,
        "alSourceStopv" => al_source_stopv,
        "alSourceRewindv" => al_source_rewindv,
        "alSourceQueueBuffers" => al_source_queue_buffers,
        "alSourceUnqueueBuffers" => al_source_unqueue_buffers,
    }
}
