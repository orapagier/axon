//! Direct Rust equivalent of the original bash/ImageMagick quote-overlay script.
//!
//! Usage:
//!   cargo run --example quote_image -- \
//!     --input background.png \
//!     --output result.png \
//!     --text "Heaven is a place where there is no pain." \
//!     --attribution "— Ellen G. White"

use image_processor::{
    text::{LoadedFont, TextAlignment, TextShadow, TextStyle},
    GradientDirection, ImagePipeline, Result,
};

struct Config {
    input: String,
    output: String,
    main_text: String,
    attribution: String,
    main_font_path: String,
    attr_font_path: Option<String>,
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str| -> Option<String> {
        args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
    };

    Config {
        input: get("--input").unwrap_or_else(|| "input.png".into()),
        output: get("--output").unwrap_or_else(|| "output.png".into()),
        main_text: get("--text")
            .unwrap_or_else(|| "In the beginning God created the heavens and the earth.".into()),
        attribution: get("--attribution").unwrap_or_else(|| "— Genesis 1:1".into()),
        main_font_path: get("--font").unwrap_or_else(|| "/fonts/Playball-Regular.ttf".into()),
        attr_font_path: get("--attr-font"),
    }
}

fn main() -> Result<()> {
    let config = parse_args();

    // Load fonts
    let main_font = LoadedFont::from_path(&config.main_font_path)?;
    let attr_font = match &config.attr_font_path {
        Some(path) => LoadedFont::from_path(path)?,
        None => LoadedFont::from_path(&config.main_font_path)?,
    };

    // Load base image
    let img = image::open(&config.input)?;
    let (width, height) = {
        use image::GenericImageView;
        img.dimensions()
    };

    let margin = (width * 5 / 100) as u32;

    // Detect text color from background
    let text_color = image_processor::utils::auto_text_color(&img, 0, 0, width, height);
    let text_color_arr = [text_color[0], text_color[1], text_color[2], 255];

    // Calculate initial font sizes based on text length (mirrors bash logic)
    let char_count = config.main_text.len();
    let line_count = config.main_text.lines().count();

    let (main_size, attr_size) = initial_font_sizes(char_count, line_count);

    // Text styles
    let main_style = TextStyle {
        size: main_size as f32,
        color: text_color_arr,
        alignment: TextAlignment::Center,
        shadow: Some(TextShadow {
            offset_x: 2,
            offset_y: 2,
            color: [0, 0, 0, 160],
        }),
        line_height: 1.4,
        ..Default::default()
    };

    let attr_style = TextStyle {
        size: attr_size as f32,
        color: text_color_arr,
        alignment: TextAlignment::Center,
        shadow: Some(TextShadow {
            offset_x: 1,
            offset_y: 1,
            color: [0, 0, 0, 140],
        }),
        line_height: 1.3,
        ..Default::default()
    };

    // Add a semi-transparent dark overlay at the bottom to improve readability
    // (equivalent to the bash script's rgba(0,0,0,0.3) background)
    ImagePipeline::new(img)
        .gradient_overlay([0, 0, 0, 0], [0, 0, 0, 100], GradientDirection::BottomToTop)
        .add_two_texts(
            &config.main_text,
            &main_font,
            &main_style,
            &config.attribution,
            &attr_font,
            &attr_style,
            margin,
            margin,
        )
        .save(&config.output)?;

    println!("{}", config.output);
    Ok(())
}

fn initial_font_sizes(char_count: usize, line_count: usize) -> (u32, u32) {
    if char_count < 50 && line_count < 2 {
        (70, 35)
    } else if char_count < 100 && line_count < 3 {
        (58, 30)
    } else if char_count < 150 && line_count < 4 {
        (50, 26)
    } else if char_count < 200 && line_count < 5 {
        (44, 24)
    } else if char_count < 250 && line_count < 6 {
        (40, 22)
    } else if char_count < 300 && line_count < 7 {
        (36, 20)
    } else if char_count < 400 && line_count < 9 {
        (32, 18)
    } else if char_count < 500 {
        (28, 16)
    } else {
        (24, 14)
    }
}
