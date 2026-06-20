// Native implementation of comix.to image XOR decoding and grid descrambling.
// Mirrors Descrambler.kt from the Kotlin reference extension.
use aidoku::{imports::canvas::{Canvas, ImageRef, Rect}, prelude::println};
use alloc::vec::Vec;

const GRID_COLS: usize = 5;
const GRID_ROWS: usize = 5;
const NUM_TILES: usize = GRID_COLS * GRID_ROWS;

const ENC_MULTIPLIER: i32 = 1000005;
const ENC_INCREMENT: i32 = 1234567891;
const LCG_MULTIPLIER: i32 = 1664525;
const LCG_INCREMENT: i32 = 1013904223;

/// XOR-decode image bytes according to x-enc-* headers.
/// `seed`   : x-enc-seed as i32
/// `length` : x-enc-len as usize
/// `algo`   : x-enc-algo value (None or Some("2"))
pub fn decode_xor(bytes: &[u8], seed: i32, length: usize, algo: Option<&str>) -> Vec<u8> {
    if algo != Some("2") {
        return decode_with_lcg(bytes, seed, length);
    }

    // algo == "2": BuildOrderV2 — xorshift with an interleaved mix variable.
    // Try 4 byte-shifts × 2 sources (state / mix) = 8 candidates.
    for &shift in &[0u32, 8, 16, 24] {
        let c = decode_with_build_order_v2(bytes, seed, length, shift, false);
        println!("[comix] decode_xor: v2 state shift={shift} bytes[0..4]={:02X?}", &c[..4.min(c.len())]);
        if has_image_signature(&c) {
            println!("[comix] decode_xor: v2 state shift={shift} matched signature");
            return c;
        }
    }
    for &shift in &[0u32, 8, 16, 24] {
        let c = decode_with_build_order_v2(bytes, seed, length, shift, true);
        println!("[comix] decode_xor: v2 mix shift={shift} bytes[0..4]={:02X?}", &c[..4.min(c.len())]);
        if has_image_signature(&c) {
            println!("[comix] decode_xor: v2 mix shift={shift} matched signature");
            return c;
        }
    }

    // Fallback to the old xorshift / LCG candidates.
    let c0 = decode_with_xorshift(bytes, seed | 1, length, false);
    if has_image_signature(&c0) { println!("[comix] decode_xor: xorshift seed|1 low matched"); return c0; }
    let c1 = decode_with_xorshift(bytes, seed, length, false);
    if has_image_signature(&c1) { println!("[comix] decode_xor: xorshift seed low matched"); return c1; }
    let c2 = decode_with_xorshift(bytes, seed | 1, length, true);
    if has_image_signature(&c2) { println!("[comix] decode_xor: xorshift seed|1 high matched"); return c2; }
    let c3 = decode_with_lcg(bytes, seed, length);
    if has_image_signature(&c3) { println!("[comix] decode_xor: lcg matched"); return c3; }

    // All candidates failed — return the original encrypted bytes so that
    // ImageRef::new() produces 0×0 dimensions and the caller's guard shows
    // a placeholder instead of crashing the layout with NaN height.
    println!("[comix] decode_xor: all candidates failed for seed={seed} algo={algo:?}, returning original bytes");
    bytes.to_vec()
}

/// Rearrange 5×5 tiles to undo the grid scrambling applied by comix.to.
/// `seed` : x-scramble-seed as i32
/// `algo` : x-scramble-algo value (None/"1"/"2" → LCG order, "3" → xorshift order)
pub fn descramble_tiles(image: &ImageRef, seed: i32, algo: Option<&str>) -> ImageRef {
    let width = image.width();
    let height = image.height();
    // Integer division ensures tile boundaries land on exact pixel boundaries —
    // fractional tile sizes accumulate rounding error that shows as gap lines.
    let tile_w = (width as usize / GRID_COLS) as f32;
    let tile_h = (height as usize / GRID_ROWS) as f32;

    let order = if algo == Some("3") {
        build_order_xorshift(seed, NUM_TILES)
    } else {
        build_order_lcg(seed, NUM_TILES)
    };

    let mut canvas = Canvas::new(width, height);
    for dst_idx in 0..NUM_TILES {
        let src_idx = order[dst_idx];
        let src_col = (src_idx % GRID_COLS) as f32;
        let src_row = (src_idx / GRID_COLS) as f32;
        let dst_col = (dst_idx % GRID_COLS) as f32;
        let dst_row = (dst_idx / GRID_COLS) as f32;
        canvas.copy_image(
            image,
            Rect::new(src_col * tile_w, src_row * tile_h, tile_w, tile_h),
            Rect::new(dst_col * tile_w, dst_row * tile_h, tile_w, tile_h),
        );
    }
    canvas.get_image()
}

// ── XOR decoders ─────────────────────────────────────────────────────────────

/// BuildOrderV2: xorshift32 with an interleaved mix variable (rotate-left-9).
/// This is the algorithm the site uses for x-enc-algo == "2".
/// `shift`   : which byte of state/mix to use as the XOR key (0, 8, 16, or 24)
/// `use_mix` : if true, XOR key comes from `mix`; if false, from `state`
fn decode_with_build_order_v2(
    bytes: &[u8],
    seed: i32,
    length: usize,
    shift: u32,
    use_mix: bool,
) -> Vec<u8> {
    let mut result = bytes.to_vec();
    let mut state = (seed as u32) | 1; // ensure non-zero
    let mut mix: u32 = 0;
    let limit = result.len().min(length);
    for byte in result.iter_mut().take(limit) {
        state ^= state << 13;
        mix = mix.wrapping_add(state);
        state ^= state >> 17;
        mix = mix.rotate_left(9) ^ state;
        state ^= state << 5;
        let key = if use_mix { (mix >> shift) as u8 } else { (state >> shift) as u8 };
        *byte ^= key;
    }
    result
}

fn decode_with_xorshift(
    bytes: &[u8],
    initial_state: i32,
    length: usize,
    high_byte: bool,
) -> Vec<u8> {
    let mut result = bytes.to_vec();
    let mut state = initial_state;
    let limit = result.len().min(length);
    for byte in result.iter_mut().take(limit) {
        state = next_xorshift_state(state);
        let key = if high_byte {
            (state as u32 >> 24) as u8
        } else {
            state as u8
        };
        *byte ^= key;
    }
    result
}

fn decode_with_lcg(bytes: &[u8], seed: i32, length: usize) -> Vec<u8> {
    let mut result = bytes.to_vec();
    let mut state = seed;
    let limit = result.len().min(length);
    for byte in result.iter_mut().take(limit) {
        state = state.wrapping_mul(ENC_MULTIPLIER).wrapping_add(ENC_INCREMENT);
        *byte ^= (state as u32 >> 24) as u8;
    }
    result
}

fn next_xorshift_state(state: i32) -> i32 {
    let mut s = state;
    s ^= s << 13;
    s ^= (s as u32 >> 17) as i32; // logical (unsigned) right shift
    s ^= s << 5;
    s
}

fn has_image_signature(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return false;
    }
    // WebP: RIFF....WEBP
    let webp = bytes[0] == b'R'
        && bytes[1] == b'I'
        && bytes[2] == b'F'
        && bytes[3] == b'F'
        && bytes[8] == b'W'
        && bytes[9] == b'E'
        && bytes[10] == b'B'
        && bytes[11] == b'P';
    // JPEG: FF D8
    let jpeg = bytes[0] == 0xFF && bytes[1] == 0xD8;
    // PNG: 89 PNG
    let png = bytes[0] == 0x89 && bytes[1] == b'P' && bytes[2] == b'N' && bytes[3] == b'G';
    webp || jpeg || png
}

// ── Tile order builders ───────────────────────────────────────────────────────

/// Build a tile order for x-scramble-algo == "3".
/// Uses BuildOrderV2 PRNG (xorshift32 + mix), then inverts — matching Swift buildShuffleOrderV3.
/// Returns the INVERSE permutation so that order[dst] = src.
fn build_order_xorshift(seed: i32, n: usize) -> Vec<usize> {
    let mut arr: Vec<usize> = (0..n).collect();
    let mut state: u32 = (seed as u32) | 1;
    let mut mix: u32 = 0;

    for i in (1..n).rev() {
        state ^= state << 13;
        mix = mix.wrapping_add(state.wrapping_mul(i as u32 + 1));
        state ^= state >> 17;
        mix = mix.rotate_left(9) ^ state;
        state ^= state << 5;
        let j = (state % (i as u32 + 1)) as usize;
        arr.swap(i, j);
    }

    inverse_permutation(&arr)
}

/// Build a tile order using a linear congruential generator (default).
/// Returns the INVERSE permutation so that order[dst] = src.
fn build_order_lcg(seed: i32, n: usize) -> Vec<usize> {
    let mut arr: Vec<usize> = (0..n).collect();
    let mut state = seed;

    for i in (1..n).rev() {
        state = state.wrapping_mul(LCG_MULTIPLIER).wrapping_add(LCG_INCREMENT);
        let j = ((state as u32 as u64) % (i as u64 + 1)) as usize;
        arr.swap(i, j);
    }

    inverse_permutation(&arr)
}

/// Given a permutation `perm` (where perm[i] = original index at position i),
/// returns its inverse (where inverse[orig] = new position).
fn inverse_permutation(perm: &[usize]) -> Vec<usize> {
    let mut inv: Vec<usize> = (0..perm.len()).map(|_| 0).collect();
    for (i, &src) in perm.iter().enumerate() {
        inv[src] = i;
    }
    inv
}
