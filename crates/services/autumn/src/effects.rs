//! Machine checks for profile effects, run on every upload to the `effects`
//! tag, so no route can skip them. A file that can't be checked is refused.
//!
//! Flash rule: WCAG 2.3.1, three flashes or below the threshold. A flash is a
//! pair of opposing changes in relative luminance of 10% or more of the maximum
//! where the darker value is below 0.8; a red flash is the same for saturated
//! red. No more than 3 in any second over an area of a quarter of a 10 degree
//! field (21,824 CSS px). An effect is an overlay, so it flashes against what
//! is behind it: every file is tested over black and over white, at the
//! largest size a profile shows it.

use std::io::Cursor;

use crate::api::Tag;

use image::{
    AnimationDecoder, DynamicImage, Frames, RgbaImage,
    codecs::{gif::GifDecoder, png::PngDecoder},
    imageops::{self, FilterType},
};
use revolt_database::CommunityImageInfo;

/// Where a file is tested: the grid for the largest size it is ever shown at.
/// The limit itself is the same everywhere, 21,824 CSS px: a smaller stage has
/// less to flash with, so the same picture may fill more of it.
#[derive(Clone, Copy)]
struct Stage {
    columns: usize,
    rows: usize,
    /// One cell of the grid, in CSS px
    cell: usize,
}

/// An effect on a profile: 450 x 800
const PROFILE: Stage = Stage { columns: 45, rows: 80, cell: 10 };
/// A decoration on the largest avatar, 208 px on the Designs wall, and the
/// picture is 1.5 times the avatar: 312 x 312
const AVATAR: Stage = Stage { columns: 39, rows: 39, cell: 8 };
/// A nameplate across the Designs wall. The band has no width limit, so this
/// is a 1920 px screen less the sidebars: 1600 wide, and the template's 16:3
/// makes it 300 high. The band crops that to about 190; the whole file counts,
/// because which part stays depends on the screen.
const NAMEPLATE: Stage = Stage { columns: 160, rows: 30, cell: 10 };

/// Decorations and nameplates share a tag, so the shape says which it is
fn design_stage(width: u32, height: u32) -> Stage {
    if width >= height * 2 { NAMEPLATE } else { AVATAR }
}

/// A file that loops plays at least this long in the test, and at least twice,
/// so the step from its last frame back to its first counts
const LOOP_TEST_MS: u32 = 2_000;
/// Browsers show a frame of 10 ms or less for this long
const SHORT_FRAME_SHOWN_MS: u32 = 100;

/// A quarter of the 341 x 256 CSS px safe area
pub const FLASH_AREA_PX: usize = 21_824;
pub const FLASH_LIMIT_PER_SECOND: usize = 3;

/// Share of the picture that may be solid in any frame: what is underneath
/// has to stay readable
pub const MAX_SOLID_SHARE: f32 = 0.5;
/// Less than this in every frame is an effect nobody can see
const MIN_SOLID_SHARE: f32 = 0.0005;

/// Why an effect was refused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectRefusal {
    /// The decoder could not read every frame, so the check could not run
    Unreadable,
    /// Over the flash limit
    Flashing,
    /// It covers too much of the profile
    TooSolid,
    /// Nothing can be seen
    Blank,
}

impl EffectRefusal {
    /// Code sent to the client with the refused field
    pub fn code(self) -> &'static str {
        match self {
            EffectRefusal::Unreadable => "unreadable",
            EffectRefusal::Flashing => "flashing",
            EffectRefusal::TooSolid => "too_solid",
            EffectRefusal::Blank => "blank",
        }
    }
}

#[derive(Clone, Copy)]
struct Sample {
    luminance: f32,
    red: f32,
}

fn channel(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Sample of a colour with channels from 0 to 1
fn sample_of(r: f32, g: f32, b: f32) -> Sample {
    let total = r + g + b;
    let redness = if total > 0.0 { r / total } else { 0.0 };
    Sample {
        luminance: 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b),
        // saturated red: at least 80% of the colour is red
        red: if redness >= 0.8 {
            (r - g - b).max(0.0) * 320.0
        } else {
            0.0
        },
    }
}

/// When the picture changed direction, by a step large enough to count
fn changes(
    samples: &[Sample],
    value: impl Fn(&Sample) -> f32,
    opposing: impl Fn(f32, f32) -> bool,
) -> Vec<(usize, f32)> {
    let mut found: Vec<(usize, f32)> = vec![];
    let mut anchor = value(&samples[0]);
    for (frame, sample) in samples.iter().enumerate().skip(1) {
        let current = value(sample);
        if opposing(anchor, current) {
            found.push((frame, (current - anchor).signum()));
            anchor = current;
        } else if found
            .last()
            .is_some_and(|(_, direction)| (current - anchor).signum() == *direction)
        {
            // still moving the same way: the extreme, not a new change
            anchor = current;
        }
    }
    found
}

/// Most flashes in any one second, from a list of changes
fn most_in_a_second(list: &[(usize, f32)], times: &[u32]) -> usize {
    // a flash needs two opposing changes
    let mut flashes = vec![];
    let mut index = 1;
    while index < list.len() {
        if list[index].1 != list[index - 1].1 {
            flashes.push(times[list[index].0]);
            index += 1;
        }
        index += 1;
    }
    flashes
        .iter()
        .map(|start| {
            flashes
                .iter()
                .filter(|time| **time >= *start && **time < start + 1000)
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// How many flashes one spot of the picture makes at most in any second.
/// `times[n]` is when sample `n` appears, in milliseconds.
fn flashes_per_second(samples: &[Sample], times: &[u32]) -> usize {
    let general = changes(
        samples,
        |s| s.luminance,
        |a, b| {
            let bright = a.max(b);
            let dark = a.min(b);
            bright - dark >= 0.1 && dark < 0.8
        },
    );
    let red = changes(samples, |s| s.red, |a, b| (a - b).abs() > 20.0);
    most_in_a_second(&general, times).max(most_in_a_second(&red, times))
}

/// One frame reduced to the test grid: premultiplied colour, alpha, pixels
struct Grid {
    colour: Vec<[f32; 3]>,
    alpha: Vec<f32>,
    pixels: Vec<f32>,
}

fn reduce(frame: &RgbaImage, stage: Stage) -> (Grid, f32) {
    // a file with fewer pixels than the grid is scaled up by every client, so
    // each of its pixels has to reach all the cells it covers on screen
    let scaled;
    let frame = if (frame.width() as usize) < stage.columns || (frame.height() as usize) < stage.rows {
        let width = frame.width().max(stage.columns as u32);
        let height = frame.height().max(stage.rows as u32);
        scaled = imageops::resize(frame, width, height, FilterType::Nearest);
        &scaled
    } else {
        frame
    };
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    let cells = stage.columns * stage.rows;
    let mut grid = Grid {
        colour: vec![[0.0; 3]; cells],
        alpha: vec![0.0; cells],
        pixels: vec![0.0; cells],
    };
    let mut solid = 0usize;
    for (y, row) in frame.as_raw().chunks_exact(width.max(1) * 4).enumerate() {
        let cell_y = y * stage.rows / height;
        for (x, pixel) in row.chunks_exact(4).enumerate() {
            let cell = cell_y * stage.columns + x * stage.columns / width;
            let alpha = pixel[3] as f32 / 255.0;
            if pixel[3] >= 128 {
                solid += 1;
            }
            for part in 0..3 {
                grid.colour[cell][part] += alpha * pixel[part] as f32 / 255.0;
            }
            grid.alpha[cell] += alpha;
            grid.pixels[cell] += 1.0;
        }
    }
    (grid, solid as f32 / (width * height).max(1) as f32)
}

/// The grid laid over a plain backdrop (0 is black, 1 is white)
fn over(grid: &Grid, backdrop: f32) -> Vec<Sample> {
    (0..grid.alpha.len())
        .map(|cell| {
            let pixels = grid.pixels[cell].max(1.0);
            let rest = (pixels - grid.alpha[cell]) * backdrop;
            let [r, g, b] = grid.colour[cell];
            sample_of((r + rest) / pixels, (g + rest) / pixels, (b + rest) / pixels)
        })
        .collect()
}

/// Largest area breaking the limit, in CSS px
fn failing_area(frames: &[Vec<Sample>], order: &[usize], times: &[u32], stage: Stage) -> usize {
    let cells = frames.first().map_or(0, Vec::len);
    let failing = (0..cells)
        .filter(|&cell| {
            let samples: Vec<Sample> = order.iter().map(|&frame| frames[frame][cell]).collect();
            flashes_per_second(&samples, times) > FLASH_LIMIT_PER_SECOND
        })
        .count();
    failing * stage.cell * stage.cell
}

/// One whole picture of an animation and how long it shows, in milliseconds
type WholeFrame = Result<(RgbaImage, u32), ()>;

fn whole(frames: Frames<'_>) -> Box<dyn Iterator<Item = WholeFrame> + '_> {
    Box::new(frames.map(|frame| {
        let frame = frame.map_err(|_| ())?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        Ok((frame.into_buffer(), numerator / denominator.max(1)))
    }))
}

fn frames_of(bytes: &[u8]) -> Option<Box<dyn Iterator<Item = WholeFrame> + '_>> {
    if bytes.starts_with(b"\x89PNG") {
        let decoder = PngDecoder::new(Cursor::new(bytes)).ok()?;
        if !decoder.is_apng().ok()? {
            return None;
        }
        Some(whole(decoder.apng().ok()?.into_frames()))
    } else if bytes.starts_with(b"GIF8") {
        Some(whole(GifDecoder::new(Cursor::new(bytes)).ok()?.into_frames()))
    } else if bytes.starts_with(b"RIFF") {
        WebpFrames::new(bytes)
    } else {
        None
    }
}

/// Frames of an animated WebP as a browser shows them. The image crate's
/// decoder never clears what a frame asks to dispose of unless it is given a
/// background colour, and it offers no way to give one: every frame would
/// appear to stay on, and a strobe would pass the flash check as a still
/// picture. So each frame is decoded alone and composed here. A browser
/// disposes to transparent, whatever colour the file names.
struct WebpFrames<'a> {
    bytes: &'a [u8],
    at: usize,
    canvas: RgbaImage,
    /// The rectangle the last frame asked to clear
    clear: Option<(u32, u32, u32, u32)>,
}

fn le24_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
}

impl<'a> WebpFrames<'a> {
    fn new(bytes: &'a [u8]) -> Option<Box<dyn Iterator<Item = WholeFrame> + 'a>> {
        let mut at = 12;
        let mut canvas = None;
        let mut animated = false;
        let mut first = None;
        while at + 8 <= bytes.len() {
            let kind = bytes.get(at..at + 4)?;
            let len = u32::from_le_bytes(bytes.get(at + 4..at + 8)?.try_into().ok()?) as usize;
            match kind {
                b"VP8X" => {
                    canvas = Some((le24_at(bytes, at + 12)? + 1, le24_at(bytes, at + 15)? + 1));
                }
                b"ANIM" => animated = true,
                b"ANMF" => {
                    first = Some(at);
                    break;
                }
                _ => {}
            }
            at = at.checked_add(8)?.checked_add(len + (len & 1))?;
        }
        let (width, height) = canvas?;
        if !animated || width as u64 * height as u64 > 16_000_000 {
            return None;
        }
        Some(Box::new(WebpFrames {
            bytes,
            at: first?,
            canvas: RgbaImage::new(width, height),
            clear: None,
        }))
    }

    /// One ANMF chunk as a WebP file of its own
    fn still(payload: &[u8], width: u32, height: u32) -> Option<RgbaImage> {
        let has_alpha = payload.starts_with(b"ALPH");
        let mut vp8x = vec![if has_alpha { 0x10u8 } else { 0 }, 0, 0, 0];
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        let mut file = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        file.extend_from_slice(&10u32.to_le_bytes());
        file.extend_from_slice(&vp8x);
        file.extend_from_slice(payload);
        let size = (file.len() - 8) as u32;
        file[4..8].copy_from_slice(&size.to_le_bytes());
        let still = image::load_from_memory_with_format(&file, image::ImageFormat::WebP).ok()?;
        Some(still.to_rgba8())
    }

    fn step(&mut self) -> Option<(RgbaImage, u32)> {
        let b = self.bytes;
        let at = self.at;
        let len = u32::from_le_bytes(b.get(at + 4..at + 8)?.try_into().ok()?) as usize;
        let data = b.get(at + 8..at.checked_add(8)?.checked_add(len)?)?;
        let (x, y) = (le24_at(data, 0)? * 2, le24_at(data, 3)? * 2);
        let (width, height) = (le24_at(data, 6)? + 1, le24_at(data, 9)? + 1);
        let duration = le24_at(data, 12)?;
        let flags = *data.get(15)?;
        if x + width > self.canvas.width() || y + height > self.canvas.height() {
            return None;
        }
        let picture = Self::still(data.get(16..)?, width, height)?;
        if picture.dimensions() != (width, height) {
            return None;
        }

        if let Some((cx, cy, cw, ch)) = self.clear.take() {
            for py in cy..cy + ch {
                for px in cx..cx + cw {
                    self.canvas.put_pixel(px, py, image::Rgba([0, 0, 0, 0]));
                }
            }
        }
        let blend = flags & 0b10 == 0;
        for (px, py, source) in picture.enumerate_pixels() {
            let target = self.canvas.get_pixel_mut(x + px, y + py);
            let a = source.0[3] as u32;
            if !blend || a == 255 {
                *target = *source;
            } else if a > 0 {
                let rest = target.0[3] as u32 * (255 - a) / 255;
                let out = a + rest;
                for part in 0..3 {
                    target.0[part] = ((source.0[part] as u32 * a + target.0[part] as u32 * rest)
                        / out.max(1)) as u8;
                }
                target.0[3] = out as u8;
            }
        }
        if flags & 0b01 != 0 {
            self.clear = Some((x, y, width, height));
        }
        self.at = at + 8 + len + (len & 1);
        Some((self.canvas.clone(), duration))
    }
}

impl Iterator for WebpFrames<'_> {
    type Item = WholeFrame;

    fn next(&mut self) -> Option<WholeFrame> {
        // browsers skip a chunk they don't know, so frames can follow it
        loop {
            let header = self.bytes.get(self.at..self.at.checked_add(8)?)?;
            if &header[..4] == b"ANMF" {
                return Some(self.step().ok_or(()));
            }
            let len = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
            self.at = self.at.checked_add(8)?.checked_add(len)?.checked_add(len & 1)?;
        }
    }
}

/// What the frames of a whole animation add up to
struct Measured {
    /// Largest area over the flash limit, over black or white, in CSS px
    flashing: usize,
    solidest: f32,
    fullest: f32,
}

/// Go through an animation on a stage. Each frame is reduced once, as it is
/// read, so a large file never sits decoded in memory; a loop repeats the
/// reduced frames.
fn measure(
    bytes: &[u8],
    info: &CommunityImageInfo,
    pick: impl Fn(u32, u32) -> Stage,
) -> Result<Measured, EffectRefusal> {
    let frames = frames_of(bytes).ok_or(EffectRefusal::Unreadable)?;
    let mut stage = None;
    let (mut black, mut white) = (vec![], vec![]);
    let mut delays = vec![];
    let mut fullest = 0f32;
    let mut solidest = 0f32;
    // an overlay appears from nothing and vanishes into nothing, which counts
    let empty = RgbaImage::new(1, 1);
    for frame in frames {
        let (buffer, delay) = frame.map_err(|_| EffectRefusal::Unreadable)?;
        let stage = *stage.get_or_insert_with(|| {
            let stage = pick(buffer.width(), buffer.height());
            let (nothing, _) = reduce(&empty, stage);
            black.push(over(&nothing, 0.0));
            white.push(over(&nothing, 1.0));
            stage
        });
        let (grid, solid) = reduce(&buffer, stage);
        solidest = solidest.max(solid);
        fullest = fullest.max(grid.alpha.iter().sum::<f32>());
        black.push(over(&grid, 0.0));
        white.push(over(&grid, 1.0));
        delays.push(if delay <= 10 { SHORT_FRAME_SHOWN_MS } else { delay });
    }
    let stage = stage.ok_or(EffectRefusal::Unreadable)?;
    // frames we did not read were not checked
    if delays.len() != info.frames as usize {
        return Err(EffectRefusal::Unreadable);
    }

    // the order the picture is seen in; 0 is the empty frame
    let (mut order, mut times) = (vec![0usize], vec![0u32]);
    let mut clock = 0u32;
    let mut passes = 0;
    loop {
        for (frame, delay) in delays.iter().enumerate() {
            order.push(frame + 1);
            times.push(clock);
            clock = clock.saturating_add(*delay);
        }
        passes += 1;
        if info.plays == 1 || (passes >= 2 && clock >= LOOP_TEST_MS) {
            break;
        }
    }
    order.push(0);
    times.push(clock);
    Ok(Measured {
        flashing: failing_area(&black, &order, &times, stage)
            .max(failing_area(&white, &order, &times, stage)),
        solidest,
        fullest,
    })
}

/// The flash and readability checks for an effect
pub fn check_effect(bytes: &[u8], info: &CommunityImageInfo) -> Result<(), EffectRefusal> {
    let seen = measure(bytes, info, |_, _| PROFILE)?;
    if seen.solidest > MAX_SOLID_SHARE {
        return Err(EffectRefusal::TooSolid);
    }
    if seen.solidest < MIN_SOLID_SHARE && seen.fullest == 0.0 {
        return Err(EffectRefusal::Blank);
    }
    if seen.flashing >= FLASH_AREA_PX {
        return Err(EffectRefusal::Flashing);
    }
    Ok(())
}

/// The check an upload needs before it goes on, by its tag
pub fn check_upload(
    tag: &Tag,
    bytes: &[u8],
    info: &CommunityImageInfo,
) -> Result<(), EffectRefusal> {
    match tag {
        Tag::effects => check_effect(bytes, info),
        Tag::decorations => check_design(bytes, info),
        _ => Ok(()),
    }
}

/// The flash check for a decoration or nameplate. A still file has nothing to
/// check; an animated one that can't be read is refused like an effect. These
/// are meant to fill their picture, so solid and blank files pass.
pub fn check_design(bytes: &[u8], info: &CommunityImageInfo) -> Result<(), EffectRefusal> {
    if info.frames < 2 {
        return Ok(());
    }
    if measure(bytes, info, design_stage)?.flashing >= FLASH_AREA_PX {
        return Err(EffectRefusal::Flashing);
    }
    Ok(())
}

/// The frame with the most on it: a still for tiles, drawn by the media
/// server, so showing a tile plays nothing and asks for nothing else
pub fn still_of(bytes: &[u8]) -> Option<DynamicImage> {
    let mut best: Option<(u64, RgbaImage)> = None;
    for frame in frames_of(bytes)? {
        let (buffer, _) = frame.ok()?;
        let weight: u64 = buffer.as_raw().chunks_exact(4).map(|p| p[3] as u64).sum();
        if best.as_ref().is_none_or(|(most, _)| weight > *most) {
            best = Some((weight, buffer));
        }
    }
    best.map(|(_, buffer)| DynamicImage::ImageRgba8(buffer))
}

fn be32(b: &[u8], at: usize) -> Option<usize> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// PNG: only what draws the picture. Text, EXIF, time stamps, colour profiles
/// and anything after the end go; frames are not touched.
fn strip_png(b: &[u8]) -> Option<Vec<u8>> {
    const KEEP: [&[u8; 4]; 11] = [
        b"IHDR", b"PLTE", b"tRNS", b"acTL", b"fcTL", b"IDAT", b"fdAT", b"gAMA", b"cHRM", b"sRGB",
        b"IEND",
    ];
    let mut out = b[..8].to_vec();
    let mut at = 8;
    loop {
        let len = be32(b, at)?;
        let end = at.checked_add(12)?.checked_add(len)?;
        let chunk = b.get(at..end)?;
        let kind = &chunk[4..8];
        if KEEP.iter().any(|keep| keep.as_slice() == kind) {
            out.extend_from_slice(chunk);
        }
        if kind == b"IEND" {
            return Some(out);
        }
        at = end;
    }
}

/// GIF: comments, text and application data go, except the loop setting,
/// which the check before this one reads. Nothing after the trailer stays.
fn strip_gif(b: &[u8]) -> Option<Vec<u8>> {
    let flags = *b.get(10)?;
    let mut at = 13;
    if flags & 0x80 != 0 {
        at += 3 * (1 << ((flags & 0x07) + 1));
    }
    let mut out = b.get(..at)?.to_vec();
    let skip_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let size = *b.get(at)? as usize;
            at += 1 + size;
            if size == 0 {
                return Some(at);
            }
        }
    };
    loop {
        match *b.get(at)? {
            0x3B => {
                out.push(0x3B);
                return Some(out);
            }
            0x21 => {
                let label = *b.get(at + 1)?;
                let end = skip_blocks(at + 2)?;
                let netscape = label == 0xFF && b.get(at + 3..at + 14) == Some(b"NETSCAPE2.0".as_slice());
                if label == 0xF9 || netscape {
                    out.extend_from_slice(b.get(at..end)?);
                }
                at = end;
            }
            0x2C => {
                let local = *b.get(at + 9)?;
                let mut end = at + 10;
                if local & 0x80 != 0 {
                    end += 3 * (1 << ((local & 0x07) + 1));
                }
                end = skip_blocks(end + 1)?;
                out.extend_from_slice(b.get(at..end)?);
                at = end;
            }
            _ => return None,
        }
    }
}

/// WebP: EXIF, XMP, colour profiles and unknown chunks go; the flags that
/// announced them are cleared and the frames are not touched
fn strip_webp(b: &[u8]) -> Option<Vec<u8>> {
    const KEEP: [&[u8; 4]; 6] = [b"VP8X", b"ANIM", b"ANMF", b"VP8 ", b"VP8L", b"ALPH"];
    const HIDDEN_FLAGS: u8 = 0x20 | 0x08 | 0x04;
    let mut out = b.get(..12)?.to_vec();
    let mut at = 12;
    while at + 8 <= b.len() {
        let kind = b.get(at..at + 4)?;
        let len = u32::from_le_bytes(b.get(at + 4..at + 8)?.try_into().ok()?) as usize;
        let padded = len + (len & 1);
        let end = at + 8 + padded;
        let chunk = b.get(at..end.min(b.len()))?;
        if KEEP.iter().any(|keep| keep.as_slice() == kind) {
            let start = out.len();
            out.extend_from_slice(chunk);
            if kind == b"VP8X" {
                *out.get_mut(start + 8)? &= !HIDDEN_FLAGS;
            }
            // browsers ignore this colour; the flash check would count it as
            // solid paint over the whole picture
            if kind == b"ANIM" {
                out.get_mut(start + 8..start + 12)?.fill(0);
            }
        }
        at = end;
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Some(out)
}

/// The same picture without data hidden in the file (P-32). Bytes are copied
/// as they are: re-encoding would lose the animation. `None` if the file can't
/// be walked to its end, which refuses it.
pub fn strip_hidden_data(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        strip_png(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        strip_gif(bytes)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        strip_webp(bytes)
    } else {
        None
    }
}

/// What the upload route knows about a file by the time it is checked
#[cfg(test)]
pub(crate) fn seen(bytes: &[u8]) -> CommunityImageInfo {
    let limits = revolt_database::CommunityImageLimits {
        max_bytes: 50_000_000,
        max_dimension: 10_000,
        max_frames: 10_000,
        max_duration_ms: 3_600_000,
    };
    revolt_database::inspect_community_image(bytes, &limits).unwrap()
}

#[cfg(test)]
mod tests {
    use image::{
        Delay, Frame, Rgba,
        codecs::gif::{GifEncoder, Repeat},
    };

    use axum::{http::StatusCode, response::IntoResponse};

    use super::*;
    use crate::api::{previews_a_still, render_design_still, to_original};

    fn design(bytes: &[u8]) -> Result<(), EffectRefusal> {
        check_design(bytes, &seen(bytes))
    }

    fn effect(bytes: &[u8]) -> Result<(), EffectRefusal> {
        check_effect(bytes, &seen(bytes))
    }

    fn design_claiming(bytes: &[u8], frames: u32) -> Result<(), EffectRefusal> {
        check_design(bytes, &CommunityImageInfo { frames, ..seen(bytes) })
    }

    /// For bytes the inspection would not get through
    fn claimed(frames: u32) -> CommunityImageInfo {
        CommunityImageInfo {
            mime: "image/gif",
            width: 90,
            height: 160,
            frames,
            duration_ms: 0,
            plays: 1,
            shortest_delay_ms: 100,
        }
    }

    const W: u32 = 90;
    const H: u32 = 160;

    /// A GIF with one play, drawn by `paint(frame, x, y)`
    fn gif(frames: u32, delay_ms: u32, paint: impl Fn(u32, u32, u32) -> Rgba<u8>) -> Vec<u8> {
        gif_of(W, H, frames, delay_ms, paint)
    }

    fn gif_of(
        w: u32,
        h: u32,
        frames: u32,
        delay_ms: u32,
        paint: impl Fn(u32, u32, u32) -> Rgba<u8>,
    ) -> Vec<u8> {
        let mut out = vec![];
        {
            let mut encoder = GifEncoder::new_with_speed(&mut out, 30);
            encoder.set_repeat(Repeat::Finite(0)).unwrap();
            for n in 0..frames {
                let buffer = RgbaImage::from_fn(w, h, |x, y| paint(n, x, y));
                encoder
                    .encode_frame(Frame::from_parts(buffer, 0, 0, Delay::from_numer_denom_ms(delay_ms, 1)))
                    .unwrap();
            }
        }
        out
    }

    fn clear() -> Rgba<u8> {
        Rgba([0, 0, 0, 0])
    }

    fn dot(frame: u32, x: u32, y: u32) -> Rgba<u8> {
        // a small white dot falling down the left edge: moves, never flashes
        if x < 8 && (y as i64 - frame as i64 * 4).abs() < 8 {
            Rgba([255, 255, 255, 255])
        } else {
            clear()
        }
    }

    #[test]
    fn a_gentle_effect_passes() {
        assert_eq!(effect(&gif(30, 100, dot)), Ok(()));
    }

    #[test]
    fn a_strobe_over_half_the_card_is_refused() {
        // half the card, white on and off, ten times a second
        let strobe = |n: u32, _x: u32, y: u32| {
            if n % 2 == 0 && y < H * 2 / 5 {
                Rgba([255, 255, 255, 255])
            } else {
                clear()
            }
        };
        assert_eq!(effect(&gif(40, 50, strobe)), Err(EffectRefusal::Flashing));
    }

    #[test]
    fn a_red_strobe_on_black_is_refused() {
        let strobe = |n: u32, _x: u32, y: u32| {
            if n % 2 == 0 && y < H * 2 / 5 {
                Rgba([255, 0, 0, 255])
            } else {
                clear()
            }
        };
        assert_eq!(effect(&gif(40, 50, strobe)), Err(EffectRefusal::Flashing));
    }

    #[test]
    fn a_slow_blink_passes() {
        // two blinks a second: under the limit of three
        let blink = |n: u32, _x: u32, y: u32| {
            if (n / 5) % 2 == 0 && y < H * 2 / 5 {
                Rgba([255, 255, 255, 255])
            } else {
                clear()
            }
        };
        assert_eq!(effect(&gif(20, 50, blink)), Ok(()));
    }

    #[test]
    fn a_strobe_too_small_to_harm_passes() {
        // a 40 by 40 CSS px strobe stays far below the area limit
        let strobe = |n: u32, x: u32, y: u32| {
            if n % 2 == 0 && x < 8 && y < 4 {
                Rgba([255, 255, 255, 255])
            } else {
                clear()
            }
        };
        assert_eq!(effect(&gif(40, 50, strobe)), Ok(()));
    }

    #[test]
    fn a_picture_covering_the_profile_is_refused() {
        let wall = |_n: u32, _x: u32, y: u32| {
            if y < H * 3 / 4 {
                Rgba([10, 200, 90, 255])
            } else {
                clear()
            }
        };
        assert_eq!(effect(&gif(10, 100, wall)), Err(EffectRefusal::TooSolid));
    }

    #[test]
    fn a_blank_file_is_refused() {
        assert_eq!(
            effect(&gif(10, 100, |_, _, _| clear())),
            Err(EffectRefusal::Blank)
        );
    }

    /// An animated WebP by hand: each frame is the band and asks to be cleared
    /// afterwards, the way an encoder makes a strobe
    fn webp_strobe(frames: u32, dispose: bool) -> Vec<u8> {
        webp_strobe_of(W, H, 60, frames, dispose)
    }

    fn webp_strobe_of(w: u32, h: u32, band_height: u32, frames: u32, dispose: bool) -> Vec<u8> {
        // the VP8L chunk of a still in plain white
        let chunk_of = |width: u32, height: u32| {
            let picture = RgbaImage::from_fn(width, height, |_, _| Rgba([255, 255, 255, 255]));
            let mut still = vec![];
            DynamicImage::ImageRgba8(picture)
                .write_to(&mut Cursor::new(&mut still), image::ImageFormat::WebP)
                .unwrap();
            let at = still.windows(4).position(|w| w == b"VP8L").unwrap();
            still[at..].to_vec()
        };
        let band = chunk_of(w, band_height);
        let speck = chunk_of(2, 2);
        let le24 = |v: u32| v.to_le_bytes()[..3].to_vec();
        let mut body = b"WEBPVP8X".to_vec();
        body.extend(10u32.to_le_bytes());
        body.extend([0x12, 0, 0, 0]);
        body.extend(le24(w - 1));
        body.extend(le24(h - 1));
        body.extend(b"ANIM");
        body.extend(6u32.to_le_bytes());
        body.extend([0, 0, 0, 0, 1, 0]);
        for n in 0..frames {
            // the band, then a speck in the corner, so the band is gone if it
            // was cleared and still there if it was not
            let (chunk, width, height, y) = if n % 2 == 0 {
                (&band, w, band_height, 0)
            } else {
                (&speck, 2, 2, (h - 2) / 2)
            };
            let mut frame = [le24(0), le24(y), le24(width - 1), le24(height - 1), le24(50)].concat();
            frame.push(u8::from(dispose));
            frame.extend_from_slice(chunk);
            if chunk.len() % 2 == 1 {
                frame.push(0);
            }
            body.extend(b"ANMF");
            body.extend((frame.len() as u32).to_le_bytes());
            body.extend(frame);
        }
        let mut out = b"RIFF".to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn a_webp_that_clears_itself_is_a_strobe() {
        // the band comes and goes: it flashes
        assert_eq!(effect(&webp_strobe(40, true)), Err(EffectRefusal::Flashing));
        // never cleared, the band just stays on
        assert_eq!(effect(&webp_strobe(40, false)), Ok(()));
    }

    #[test]
    fn a_file_that_cannot_be_decoded_is_refused() {
        assert_eq!(check_effect(b"GIF89a broken", &claimed(2)), Err(EffectRefusal::Unreadable));
        assert_eq!(check_effect(b"not an image", &claimed(2)), Err(EffectRefusal::Unreadable));
    }

    fn crc(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in data {
            crc ^= *byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        !crc
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
        out
    }

    /// Chunks of an encoded still PNG, in order
    fn png_chunks(bytes: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut at = 8;
        let mut out = vec![];
        while at < bytes.len() {
            let len = be32(bytes, at).unwrap();
            out.push((bytes[at + 4..at + 8].to_vec(), bytes[at + 8..at + 8 + len].to_vec()));
            at += 12 + len;
        }
        out
    }

    /// An APNG with two frames, built from still PNGs
    fn apng(extra: &[Vec<u8>]) -> Vec<u8> {
        let still = |shade: u8| {
            let mut out = vec![];
            let image = RgbaImage::from_fn(W, H, |x, _| {
                if x < 8 { Rgba([shade, shade, shade, 255]) } else { clear() }
            });
            DynamicImage::ImageRgba8(image)
                .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            png_chunks(&out)
        };
        let first = still(255);
        let second = still(200);
        let control = |sequence: u32| {
            let mut data = sequence.to_be_bytes().to_vec();
            data.extend(W.to_be_bytes());
            data.extend(H.to_be_bytes());
            data.extend([0u8; 8]);
            data.extend(100u16.to_be_bytes());
            data.extend(1000u16.to_be_bytes());
            data.extend([0u8, 0]);
            data
        };
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend(chunk(b"IHDR", &first[0].1));
        let mut actl = 2u32.to_be_bytes().to_vec();
        actl.extend(1u32.to_be_bytes());
        out.extend(chunk(b"acTL", &actl));
        for piece in extra {
            out.extend_from_slice(piece);
        }
        out.extend(chunk(b"fcTL", &control(0)));
        for (kind, data) in first.iter().filter(|(kind, _)| kind == b"IDAT") {
            out.extend(chunk(kind.as_slice().try_into().unwrap(), data));
        }
        out.extend(chunk(b"fcTL", &control(1)));
        let mut sequence = 2u32;
        for (_, data) in second.iter().filter(|(kind, _)| kind == b"IDAT") {
            let mut body = sequence.to_be_bytes().to_vec();
            body.extend_from_slice(data);
            out.extend(chunk(b"fdAT", &body));
            sequence += 1;
        }
        out.extend(chunk(b"IEND", &[]));
        out
    }

    /// An APNG that is `on` or clear, frame by frame, 50 ms each
    fn apng_of(w: u32, h: u32, frames: u32, on: impl Fn(u32) -> bool) -> Vec<u8> {
        let idat = |lit: bool| {
            let image = RgbaImage::from_fn(w, h, |_, _| {
                if lit { Rgba([255, 255, 255, 255]) } else { clear() }
            });
            let mut out = vec![];
            DynamicImage::ImageRgba8(image)
                .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            png_chunks(&out)
        };
        let (lit, dark) = (idat(true), idat(false));
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend(chunk(b"IHDR", &lit[0].1));
        let mut actl = frames.to_be_bytes().to_vec();
        actl.extend(1u32.to_be_bytes());
        out.extend(chunk(b"acTL", &actl));
        let mut sequence = 0u32;
        for n in 0..frames {
            let mut control = sequence.to_be_bytes().to_vec();
            sequence += 1;
            control.extend(w.to_be_bytes());
            control.extend(h.to_be_bytes());
            control.extend([0u8; 8]);
            control.extend(50u16.to_be_bytes());
            control.extend(1000u16.to_be_bytes());
            control.extend([0u8, 0]);
            out.extend(chunk(b"fcTL", &control));
            let picture = if on(n) { &lit } else { &dark };
            for (_, data) in picture.iter().filter(|(kind, _)| kind == b"IDAT") {
                if n == 0 {
                    out.extend(chunk(b"IDAT", data));
                } else {
                    let mut body = sequence.to_be_bytes().to_vec();
                    sequence += 1;
                    body.extend_from_slice(data);
                    out.extend(chunk(b"fdAT", &body));
                }
            }
        }
        out.extend(chunk(b"IEND", &[]));
        out
    }

    const SHAPES: [(&str, u32, u32); 2] = [("decoration", 128, 128), ("nameplate", 256, 48)];

    #[test]
    fn a_strobing_design_is_refused_in_every_format() {
        for (name, w, h) in SHAPES {
            let white = Rgba([255, 255, 255, 255]);
            let refused = Err(EffectRefusal::Flashing);
            let on_off = |n: u32, _: u32, _: u32| if n % 2 == 0 { white } else { clear() };
            assert_eq!(design(&gif_of(w, h, 20, 50, on_off)), refused, "gif {name}");
            assert_eq!(design(&apng_of(w, h, 20, |n| n % 2 == 0)), refused, "apng {name}");
            assert_eq!(design(&webp_strobe_of(w, h, h, 20, true)), refused, "webp {name}");
        }
    }

    #[test]
    fn a_calm_design_passes_in_every_format() {
        for (name, w, h) in SHAPES {
            let white = Rgba([255, 255, 255, 255]);
            // two changes a second: under the limit
            let slow = |n: u32, _: u32, _: u32| if (n / 5) % 2 == 0 { white } else { clear() };
            assert_eq!(design(&gif_of(w, h, 20, 50, slow)), Ok(()), "gif {name}");
            assert_eq!(design(&apng_of(w, h, 20, |n| (n / 5) % 2 == 0)), Ok(()), "apng {name}");
            assert_eq!(design(&webp_strobe_of(w, h, h, 20, false)), Ok(()), "webp {name}");
        }
    }

    #[test]
    fn a_still_design_passes_unread_and_a_solid_one_is_fine() {
        let mut still = vec![];
        DynamicImage::ImageRgba8(RgbaImage::from_fn(128, 128, |_, _| Rgba([9, 200, 90, 255])))
            .write_to(&mut Cursor::new(&mut still), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(design(&still), Ok(()));
        assert_eq!(check_design(b"not read at all", &claimed(1)), Ok(()));
        // a design may fill its picture, unlike an effect
        let solid = |_: u32, _: u32, _: u32| Rgba([9, 200, 90, 255]);
        assert_eq!(design(&gif_of(128, 128, 10, 100, solid)), Ok(()));
    }

    #[test]
    fn the_same_share_of_the_picture_is_judged_by_the_size_it_is_shown_at() {
        let white = Rgba([255, 255, 255, 255]);
        // an eighth of the picture: 12,168 px on a decoration, 60,000 on a nameplate
        let eighth = |w: u32, h: u32| {
            move |n: u32, x: u32, y: u32| {
                if n % 2 == 0 && x < w / 2 && y < h / 4 { white } else { clear() }
            }
        };
        assert_eq!(design(&gif_of(128, 128, 20, 50, eighth(128, 128))), Ok(()));
        assert_eq!(
            design(&gif_of(256, 48, 20, 50, eighth(256, 48))),
            Err(EffectRefusal::Flashing)
        );
        // a third of a decoration is over the limit too, 32,448 px
        let third = |n: u32, x: u32, _: u32| if n % 2 == 0 && x < 43 { white } else { clear() };
        assert_eq!(design(&gif_of(128, 128, 20, 50, third)), Err(EffectRefusal::Flashing));
        // a sprinkle on a nameplate stays under it, 12,800 px
        let sprinkle = |n: u32, x: u32, y: u32| {
            if n % 2 == 0 && x < 26 && y < 12 { white } else { clear() }
        };
        assert_eq!(design(&gif_of(256, 48, 20, 50, sprinkle)), Ok(()));
    }

    #[test]
    fn each_tag_gets_its_own_check() {
        let white = Rgba([255, 255, 255, 255]);
        let strobe = gif_of(128, 128, 20, 50, |n, _, _| if n % 2 == 0 { white } else { clear() });
        let mut still = vec![];
        DynamicImage::ImageRgba8(RgbaImage::from_fn(128, 128, |_, _| white))
            .write_to(&mut Cursor::new(&mut still), image::ImageFormat::Png)
            .unwrap();

        // the file is not a shape an effect may have, but the machine check is the same
        assert_eq!(check_upload(&Tag::decorations, &strobe, &seen(&strobe)), Err(EffectRefusal::Flashing));
        assert_eq!(check_upload(&Tag::effects, &strobe, &seen(&strobe)), Err(EffectRefusal::TooSolid));
        assert_eq!(check_upload(&Tag::decorations, &still, &seen(&still)), Ok(()));
        assert_eq!(check_upload(&Tag::effects, &still, &seen(&still)), Err(EffectRefusal::Unreadable));
        assert_eq!(check_upload(&Tag::attachments, &strobe, &seen(&strobe)), Ok(()));
    }

    /// `gif_of`, playing for ever
    fn looping_gif(
        w: u32,
        h: u32,
        frames: u32,
        delay_ms: u32,
        paint: impl Fn(u32, u32, u32) -> Rgba<u8>,
    ) -> Vec<u8> {
        let mut out = vec![];
        {
            let mut encoder = GifEncoder::new_with_speed(&mut out, 30);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for n in 0..frames {
                let buffer = RgbaImage::from_fn(w, h, |x, y| paint(n, x, y));
                encoder
                    .encode_frame(Frame::from_parts(buffer, 0, 0, Delay::from_numer_denom_ms(delay_ms, 1)))
                    .unwrap();
            }
        }
        out
    }

    fn white_then_black(n: u32, _: u32, _: u32) -> Rgba<u8> {
        if n % 2 == 0 { Rgba([255, 255, 255, 255]) } else { Rgba([0, 0, 0, 255]) }
    }

    /// Change the value right after the acTL frame count: how many plays
    fn apng_plays_forever(mut file: Vec<u8>) -> Vec<u8> {
        let at = file.windows(4).position(|w| w == b"acTL").unwrap();
        file[at + 8..at + 12].copy_from_slice(&0u32.to_be_bytes());
        let body = file[at..at + 12].to_vec();
        let sum = crc(&body);
        file[at + 12..at + 16].copy_from_slice(&sum.to_be_bytes());
        file
    }

    fn webp_plays_forever(mut file: Vec<u8>) -> Vec<u8> {
        let at = file.windows(4).position(|w| w == b"ANIM").unwrap();
        file[at + 12..at + 14].copy_from_slice(&[0, 0]);
        file
    }

    #[test]
    fn a_short_loop_flashes_across_its_seam() {
        for (name, w, h) in SHAPES {
            let gif = looping_gif(w, h, 6, 80, white_then_black);
            assert_eq!(design(&gif), Err(EffectRefusal::Flashing), "gif {name}");
            let apng = apng_plays_forever(apng_of(w, h, 6, |n| n % 2 == 0));
            assert_eq!(design(&apng), Err(EffectRefusal::Flashing), "apng {name}");
            let webp = webp_plays_forever(webp_strobe_of(w, h, h, 6, true));
            assert_eq!(design(&webp), Err(EffectRefusal::Flashing), "webp {name}");
        }
    }

    #[test]
    fn the_seam_alone_can_be_what_flashes() {
        // two changes in a pass, and a third back to white at the seam
        let paint = |n: u32, x: u32, y: u32| white_then_black(n.min(1), x, y);
        assert_eq!(design(&looping_gif(128, 128, 2, 120, paint)), Err(EffectRefusal::Flashing));
    }

    #[test]
    fn the_same_frames_played_once_are_not_a_loop() {
        assert_eq!(design(&gif_of(128, 128, 6, 80, white_then_black)), Ok(()));
    }

    #[test]
    fn a_calm_loop_stays_calm() {
        let slow = |n: u32, x: u32, y: u32| white_then_black(n / 5, x, y);
        assert_eq!(design(&looping_gif(128, 128, 20, 100, slow)), Ok(()));
    }

    #[test]
    fn a_loop_with_no_delay_ends_and_is_judged_as_a_browser_plays_it() {
        // browsers show a frame of 10 ms or less for 100 ms: five flashes a second
        assert_eq!(
            design(&looping_gif(128, 128, 2, 0, white_then_black)),
            Err(EffectRefusal::Flashing)
        );
    }

    #[test]
    fn a_tiny_file_counts_at_the_size_it_is_shown() {
        let white = Rgba([255, 255, 255, 255]);
        let strobe = |n: u32, _: u32, _: u32| if n % 2 == 0 { white } else { clear() };
        assert_eq!(design(&gif_of(18, 18, 20, 50, strobe)), Err(EffectRefusal::Flashing));
        assert_eq!(design(&gif_of(40, 5, 20, 50, strobe)), Err(EffectRefusal::Flashing));
        // a stripe, so it stays under the solid limit of an effect
        let stripe = |n: u32, x: u32, _: u32| if n % 2 == 0 && x < 4 { white } else { clear() };
        assert_eq!(effect(&gif_of(9, 16, 20, 50, stripe)), Err(EffectRefusal::Flashing));
    }

    /// A chunk browsers skip, put right after the first frame
    fn with_unknown_chunk(file: Vec<u8>) -> Vec<u8> {
        let first = file.windows(4).position(|w| w == b"ANMF").unwrap();
        let len = u32::from_le_bytes(file[first + 4..first + 8].try_into().unwrap()) as usize;
        let after = first + 8 + len + (len & 1);
        let mut out = file[..after].to_vec();
        out.extend(b"JUNK");
        out.extend(4u32.to_le_bytes());
        out.extend([1, 2, 3, 4]);
        out.extend_from_slice(&file[after..]);
        let size = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&size.to_le_bytes());
        out
    }

    #[test]
    fn an_unknown_webp_chunk_hides_no_frames() {
        let hidden = with_unknown_chunk(webp_strobe_of(128, 128, 128, 20, true));
        assert_eq!(design(&hidden), Err(EffectRefusal::Flashing));
        assert_eq!(effect(&with_unknown_chunk(webp_strobe(40, true))), Err(EffectRefusal::Flashing));
    }

    #[test]
    fn frames_that_do_not_add_up_are_refused() {
        let file = gif_of(128, 128, 6, 200, white_then_black);
        assert_eq!(design_claiming(&file, 7), Err(EffectRefusal::Unreadable));
        assert_eq!(design_claiming(&file, 5), Err(EffectRefusal::Unreadable));
    }

    #[test]
    fn an_apng_that_claims_one_frame_is_not_a_still() {
        let mut file = apng_of(128, 128, 20, |n| n % 2 == 0);
        let at = file.windows(4).position(|w| w == b"acTL").unwrap();
        file[at + 4..at + 8].copy_from_slice(&1u32.to_be_bytes());
        let body = file[at..at + 12].to_vec();
        let sum = crc(&body);
        file[at + 12..at + 16].copy_from_slice(&sum.to_be_bytes());
        assert!(design(&file).is_err());
    }

    #[tokio::test]
    async fn the_preview_of_an_animated_design_is_one_frame_in_its_own_shape() {
        let nameplate = [
            gif_of(1024, 192, 4, 100, white_then_black),
            apng_of(1024, 192, 4, |n| n % 2 == 0),
            webp_strobe_of(1024, 192, 192, 4, true),
        ];
        for file in nameplate {
            let preview = render_design_still(file, "decorations").await.unwrap();
            assert_eq!(&preview[8..12], b"WEBP");
            assert!(!preview.windows(4).any(|w| w == b"ANMF"));
            let image = image::load_from_memory(&preview).unwrap().to_rgba8();
            assert_eq!(image.dimensions(), (512, 96));
            // the fullest frame, not an empty one
            assert!(image.get_pixel(256, 48).0[3] > 200);
        }
        let square = render_design_still(webp_strobe_of(512, 512, 512, 4, true), "decorations");
        let image = image::load_from_memory(&square.await.unwrap()).unwrap();
        assert_eq!((image.width(), image.height()), (512, 512));
    }

    #[test]
    fn designs_preview_as_a_still_and_never_answer_with_a_lasting_redirect() {
        assert!(previews_a_still(&Tag::decorations));
        assert!(previews_a_still(&Tag::effects));
        assert!(!previews_a_still(&Tag::attachments));
        for tag in [Tag::decorations, Tag::effects] {
            let answer = to_original(&tag, "/a/b/c").into_response();
            assert_eq!(answer.status(), StatusCode::TEMPORARY_REDIRECT);
        }
        let answer = to_original(&Tag::attachments, "/a/b/c").into_response();
        assert_eq!(answer.status(), StatusCode::PERMANENT_REDIRECT);
    }

    #[test]
    fn an_animated_design_that_cannot_be_read_is_refused() {
        assert_eq!(check_design(b"GIF89a broken", &claimed(20)), Err(EffectRefusal::Unreadable));
    }

    #[test]
    fn a_small_strobe_on_a_design_passes_and_the_stage_goes_by_shape() {
        // a corner of a decoration, 5% of 312 x 312: under 21,824 CSS px
        let white = Rgba([255, 255, 255, 255]);
        let corner = |n: u32, x: u32, y: u32| {
            if n % 2 == 0 && x < 28 && y < 28 { white } else { clear() }
        };
        assert_eq!(design(&gif_of(128, 128, 20, 50, corner)), Ok(()));
        assert_eq!(design_stage(1024, 192).columns, NAMEPLATE.columns);
        assert_eq!(design_stage(1024, 1024).columns, AVATAR.columns);
    }

    #[test]
    fn hidden_data_leaves_a_png_and_the_animation_stays() {
        let secret = b"Comment\0lives at 47.07 15.44, Pixel 9";
        let file = {
            let mut file = apng(&[chunk(b"tEXt", secret), chunk(b"eXIf", b"MM\0*GPS"), chunk(b"tIME", &[0; 7])]);
            file.extend_from_slice(b"appended after the end");
            file
        };
        assert!(file.windows(5).any(|w| w == b"47.07"));
        let clean = strip_hidden_data(&file).unwrap();
        assert!(!clean.windows(5).any(|w| w == b"47.07"));
        assert!(!clean.windows(4).any(|w| w == b"tEXt" || w == b"eXIf" || w == b"tIME"));
        assert!(!clean.windows(8).any(|w| w == b"appended"));
        assert_eq!(effect(&clean), Ok(()));
        assert_eq!(frames_of(&clean).unwrap().count(), 2);
    }

    #[test]
    fn hidden_data_leaves_a_gif_and_the_frames_stay() {
        let plain = gif(6, 100, dot);
        // a comment block goes in just before the trailer
        let mut file = plain[..plain.len() - 1].to_vec();
        file.extend_from_slice(&[0x21, 0xFE, 5]);
        file.extend_from_slice(b"Alice");
        file.extend_from_slice(&[0, 0x3B]);
        file.extend_from_slice(b"appended");
        let clean = strip_hidden_data(&file).unwrap();
        assert!(!clean.windows(5).any(|w| w == b"Alice"));
        assert!(!clean.windows(8).any(|w| w == b"appended"));
        assert_eq!(frames_of(&clean).unwrap().count(), 6);
    }

    #[test]
    fn hidden_data_leaves_a_webp() {
        let vp8x = {
            let mut data = vec![0x08u8 | 0x04 | 0x10, 0, 0, 0];
            data.extend([89, 0, 0, 159, 0, 0]);
            data
        };
        let riff_chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut out = kind.to_vec();
            out.extend((data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(0);
            }
            out
        };
        let mut file = b"RIFF\0\0\0\0WEBP".to_vec();
        file.extend(riff_chunk(b"VP8X", &vp8x));
        file.extend(riff_chunk(b"ANIM", &[255, 255, 255, 255, 1, 0]));
        file.extend(riff_chunk(b"ANMF", &[0; 20]));
        file.extend(riff_chunk(b"EXIF", b"GPS 47.07"));
        file.extend(riff_chunk(b"XMP ", b"<x:creator>Alice</x:creator>"));
        let clean = strip_hidden_data(&file).unwrap();
        assert!(!clean.windows(5).any(|w| w == b"47.07" || w == b"Alice"));
        assert_eq!(clean[20] & 0x0C, 0, "EXIF and XMP flags cleared");
        assert_eq!(clean[20] & 0x10, 0x10, "alpha flag stays");
        let anim = clean.windows(4).position(|w| w == b"ANIM").unwrap();
        assert_eq!(&clean[anim + 8..anim + 14], &[0, 0, 0, 0, 1, 0], "no solid background");
        let size = u32::from_le_bytes(clean[4..8].try_into().unwrap()) as usize;
        assert_eq!(size + 8, clean.len());
    }

    #[test]
    fn a_file_that_is_not_an_image_cannot_be_stripped() {
        assert_eq!(strip_hidden_data(b"<svg onload=alert(1)>"), None);
        // cut short: the walk fails, so the file is refused
        let file = apng(&[]);
        assert_eq!(strip_hidden_data(&file[..file.len() - 20]), None);
    }

    #[test]
    fn the_still_is_the_frame_with_the_most_on_it() {
        let grow = |n: u32, x: u32, _y: u32| {
            if x < n * 4 { Rgba([255, 255, 255, 255]) } else { clear() }
        };
        let still = still_of(&gif(5, 100, grow)).unwrap().to_rgba8();
        assert_eq!(still.get_pixel(12, 80).0[3], 255);
        assert_eq!(still.get_pixel(20, 80).0[3], 0);
    }
}
