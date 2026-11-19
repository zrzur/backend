//! Small previews, for people who save data (`GET /{tag}/{file_id}?size=small`).
//!
//! The ordinary preview is made for a wide screen: 1280 px for an attachment,
//! 256 px for an avatar. On a phone that pays for every megabyte, a picture in
//! a chat is shown about 300 px wide and an avatar at 40. The small preview is
//! sized for that, and it is always one still frame: a GIF or an animated
//! emoji answers with its first frame instead of the whole file, so a chat
//! can show what waits behind a tap without downloading it.
//!
//! Anything but `size=small` is the ordinary preview, so an app that never
//! heard of this is served exactly as before.

use std::io::Cursor;

use image::{DynamicImage, imageops::FilterType};
use revolt_files::decode_image;
use revolt_result::{Result, create_error};
use serde::Deserialize;

/// WebP quality of small previews. Lower than the ordinary 90: at these
/// sizes the difference is not seen, and the file is about a third smaller.
pub const SMALL_QUALITY: f32 = 70.0;

/// The query of the preview route
#[derive(Deserialize, Default)]
pub struct PreviewQuery {
    pub size: Option<String>,
}

impl PreviewQuery {
    /// Whether the small preview was asked for
    pub fn small(&self) -> bool {
        self.size.as_deref() == Some("small")
    }
}

/// Largest width and height of a small preview. Twice what a phone shows,
/// so it stays sharp on dense screens; the shape is always kept.
pub fn small_bounds(tag: &str) -> [u32; 2] {
    match tag {
        "attachments" => [480, 480],
        "avatars" | "icons" => [96, 96],
        "banners" => [640, 640],
        "backgrounds" => [640, 360],
        "emojis" => [64, 64],
        "decorations" => [192, 192],
        "effects" => [128, 228],
        // a tag added later gets a safe size instead of a panic
        _ => [256, 256],
    }
}

/// The cache key of a preview: the small one of a file is its own entry
pub fn preview_key(tag: &str, hash: &str, small: bool) -> String {
    if small {
        format!("{tag}:small:{hash}")
    } else {
        format!("{tag}:{hash}")
    }
}

/// Shrink a picture into its small preview. Never enlarges.
pub fn encode_small(image: DynamicImage, tag: &str) -> Vec<u8> {
    let [w, h] = small_bounds(tag);
    let image = image.resize(
        image.width().min(w),
        image.height().min(h),
        FilterType::Lanczos3,
    );
    let image: DynamicImage = if image.has_alpha() {
        image.to_rgba8().into()
    } else {
        image.to_rgb8().into()
    };
    webp::Encoder::from_image(&image)
        .expect("an 8-bit RGB or RGBA picture")
        .encode(SMALL_QUALITY)
        .to_vec()
}

/// Read a file and make its small preview, away from the threads that answer
/// requests. Of an animated file the first frame is taken: reading every
/// frame of a long GIF to pick the fullest one would cost far more than the
/// preview is worth.
pub async fn render_small(
    original: Vec<u8>,
    content_type: String,
    tag: &'static str,
) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let image = decode_image(&mut Cursor::new(original), &content_type)?;
        Ok(encode_small(image, tag))
    })
    .await
    .map_err(|_| create_error!(InternalError))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn query(size: Option<&str>) -> PreviewQuery {
        PreviewQuery { size: size.map(str::to_owned) }
    }

    fn size_of(webp_bytes: &[u8]) -> (u32, u32) {
        let image = image::load_from_memory(webp_bytes).expect("a WebP picture");
        (image.width(), image.height())
    }

    /// A picture with detail in it, so sizes compare like real photos
    fn photo(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
            Rgb([
                ((x * 7 + y * 3) % 256) as u8,
                ((x * x / 9 + y) % 256) as u8,
                ((x + y * y / 11) % 256) as u8,
            ])
        }))
    }

    #[test]
    fn only_the_word_small_asks_for_it() {
        assert!(query(Some("small")).small());
        assert!(!query(None).small());
        assert!(!query(Some("")).small());
        assert!(!query(Some("Small")).small());
        assert!(!query(Some("tiny")).small());
        assert!(!query(Some("1")).small());
    }

    #[test]
    fn every_kind_of_file_has_a_smaller_size_than_its_ordinary_preview() {
        // the ordinary sizes of Revolt.toml, [files.preview]
        let ordinary = [
            ("attachments", [1280, 1280]),
            ("avatars", [256, 256]),
            ("backgrounds", [1280, 720]),
            ("icons", [256, 256]),
            ("banners", [1920, 1920]),
            ("emojis", [128, 128]),
            ("decorations", [512, 512]),
            ("effects", [256, 456]),
        ];
        for (tag, [w, h]) in ordinary {
            let [sw, sh] = small_bounds(tag);
            assert!(sw < w && sh < h, "{tag}: {sw}x{sh} is not under {w}x{h}");
        }
        assert_eq!(small_bounds("something-new"), [256, 256]);
    }

    #[test]
    fn the_small_preview_has_its_own_cache_entry() {
        assert_eq!(preview_key("attachments", "abc", false), "attachments:abc");
        assert_eq!(preview_key("attachments", "abc", true), "attachments:small:abc");
        assert_ne!(preview_key("avatars", "abc", true), preview_key("avatars", "abc", false));
    }

    #[test]
    fn a_camera_picture_fits_480_and_keeps_its_shape() {
        let wide = encode_small(photo(4032, 3024), "attachments");
        assert_eq!(size_of(&wide), (480, 360));
        let tall = encode_small(photo(3024, 4032), "attachments");
        assert_eq!(size_of(&tall), (360, 480));
    }

    #[test]
    fn a_picture_that_is_already_small_is_not_enlarged() {
        let small = encode_small(photo(200, 100), "attachments");
        assert_eq!(size_of(&small), (200, 100));
        let emoji = encode_small(photo(48, 48), "emojis");
        assert_eq!(size_of(&emoji), (48, 48));
    }

    #[test]
    fn avatars_emoji_and_banners_get_their_own_sizes() {
        assert_eq!(size_of(&encode_small(photo(512, 512), "avatars")), (96, 96));
        assert_eq!(size_of(&encode_small(photo(512, 512), "icons")), (96, 96));
        assert_eq!(size_of(&encode_small(photo(128, 128), "emojis")), (64, 64));
        assert_eq!(size_of(&encode_small(photo(1920, 1080), "banners")), (640, 360));
    }

    #[test]
    fn it_is_much_lighter_than_a_1280_preview() {
        let original = photo(4032, 3024);
        let ordinary = {
            let fitted = original.resize(1280, 1280, FilterType::Lanczos3).to_rgb8();
            webp::Encoder::from_image(&fitted.into()).unwrap().encode(90.0).to_vec()
        };
        let small = encode_small(original, "attachments");
        assert!(
            small.len() * 4 < ordinary.len(),
            "small {} bytes, ordinary {} bytes",
            small.len(),
            ordinary.len()
        );
    }

    #[test]
    fn see_through_parts_stay_see_through() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(128, 128, |x, _| {
            if x < 64 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) }
        }));
        let small = image::load_from_memory(&encode_small(image, "emojis")).unwrap().to_rgba8();
        assert_eq!(small.get_pixel(8, 32).0[3], 255);
        assert_eq!(small.get_pixel(56, 32).0[3], 0);
    }

    #[test]
    fn sixteen_bit_and_grey_pictures_are_taken_too() {
        let grey = DynamicImage::ImageLuma8(image::GrayImage::from_pixel(300, 300, image::Luma([90])));
        assert_eq!(size_of(&encode_small(grey, "avatars")), (96, 96));
        let deep = DynamicImage::ImageRgb16(image::ImageBuffer::from_pixel(300, 300, Rgb([900u16, 20_000, 60_000])));
        assert_eq!(size_of(&encode_small(deep, "avatars")), (96, 96));
    }

    #[tokio::test]
    async fn a_gif_answers_with_one_still_frame() {
        use image::{Delay, Frame, codecs::gif::GifEncoder};
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            for shade in [10u8, 120, 240] {
                let frame = RgbaImage::from_pixel(600, 300, Rgba([shade, shade, shade, 255]));
                encoder
                    .encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(100, 1)))
                    .unwrap();
            }
        }
        let small = render_small(bytes, "image/gif".to_owned(), "attachments").await.unwrap();
        // a WebP of one frame, fitted to 480: not the GIF itself
        assert_eq!(&small[..4], b"RIFF");
        assert_eq!(&small[8..12], b"WEBP");
        assert_eq!(size_of(&small), (480, 240));
        let first = image::load_from_memory(&small).unwrap().to_rgb8();
        assert!(first.get_pixel(10, 10).0[0] < 40, "not the first frame");
    }

    #[tokio::test]
    async fn a_file_that_is_no_picture_is_an_error_not_a_crash() {
        assert!(render_small(b"not a picture".to_vec(), "image/png".to_owned(), "attachments").await.is_err());
    }
}
