//! Teaching the budget what a picture costs, without the runtime learning a price list.
//!
//! `BytesPerToken` refuses to put a figure on a [`Content::Blob`]. Its payload is base64, and
//! base64 over four is a number about an encoding rather than about a model: a 400 KB screenshot
//! would arrive as a hundred thousand tokens and send a compactor after a context that is nowhere
//! near full. What a picture really costs is a formula over its *dimensions*, every vendor
//! publishes one, and each publishes a different one - so the runtime carries none of them.
//!
//! What it carries instead is the two halves that let you supply one:
//!
//! - [`Blob::meta`], a free-form value the kernel never reads, for whatever a counter would need
//!   in order to price the payload. The caller who encoded the PNG had it decoded a moment
//!   earlier, so they are the one who knows.
//! - [`TokenCounter::uncounted`], so that a counter which *cannot* price something says so out
//!   loud instead of returning `0` and letting it read as free.
//!
//! This is the program that uses them. It counts the same context three ways - with the default
//! counter, with one that knows a vendor formula, and with one that knows the formula but is
//! handed a blob nobody measured - and prints what each of them makes of it.
//!
//! ```text
//! cargo run --example pricing_a_picture
//! ```

use std::sync::Arc;

use nachalnik::{Blob, BytesPerToken, Config, Content, ContextItem, Kernel, TokenCounter};
use serde_json::json;

const WIDTH: usize = 78;

/// A counter that prices a picture from its dimensions, and text the way the default one does.
///
/// note: the formula is one real vendor's, written out rather than abstracted, because the point
/// of this example is that *you* write it. Another vendor charges width times height over 750,
/// and a third charges a flat 258 a tile; there is no shape all three share, which is why this
/// lives in an example instead of in the crate.
///
/// note: it wraps `BytesPerToken` rather than replacing it. Everything that is not a picture is
/// somebody else's problem already solved, and a counter that reimplemented text counting to add
/// image counting would be two decisions in one type.
struct Tiled {
    /// What the vendor charges just for being shown an image.
    base: usize,
    /// What it charges for each tile the image is cut into.
    per_tile: usize,
    /// How big a tile is, in pixels.
    tile: u32,
    /// The counter that handles everything that is not a picture.
    text: BytesPerToken,
}

impl Default for Tiled {
    /// The numbers one vendor publishes: 85 to look at it, 170 a tile, 512-pixel tiles.
    fn default() -> Self {
        Self {
            base: 85,
            per_tile: 170,
            tile: 512,
            text: BytesPerToken::default(),
        }
    }
}

impl Tiled {
    /// What this vendor would charge for a picture of these dimensions.
    fn tiles(&self, w: u32, h: u32) -> usize {
        let across = w.div_ceil(self.tile) as usize;
        let down = h.div_ceil(self.tile) as usize;

        self.base + self.per_tile * across * down
    }

    /// The dimensions the producer recorded, if it recorded any.
    ///
    /// note: `None` is the case that matters, and it is why `uncounted` below is not simply
    /// "how many blobs are there". A blob nobody measured cannot be priced by any formula, and
    /// this counter is in exactly the position the default one is always in: it has to say so.
    fn measured(blob: &Blob) -> Option<(u32, u32)> {
        let w = blob.meta.get("w")?.as_u64()? as u32;
        let h = blob.meta.get("h")?.as_u64()? as u32;

        Some((w, h))
    }
}

impl TokenCounter for Tiled {
    fn name(&self) -> &'static str {
        "Tiled(85 + 170/tile)"
    }

    fn count(&self, content: &Content) -> usize {
        let pictures: usize = content
            .blobs()
            .iter()
            .filter_map(|blob| Self::measured(blob))
            .map(|(w, h)| self.tiles(w, h))
            .sum();

        // the text side is the default counter's answer, which already subtracts every blob's
        // bytes before dividing - so this adds the pictures to prose rather than to base64
        self.text.count(content) + pictures
    }

    fn uncounted(&self, content: &Content) -> usize {
        content
            .blobs()
            .iter()
            .filter(|blob| Self::measured(blob).is_none())
            .count()
    }
}

/// A 48x48 PNG, base64, as a caller would hand one over.
const SQUARE: &str = "iVBORw0KGgoAAAANSUhEUgAAADAAAAAwCAIAAADYYG7QAAAAPElEQVR42u3OsQkAAAgEsd9/ad1BLAQ\
                      DVx9JJacKEBAQEBAQEBAQEBAQ0Gy0dQICAgICAgICAgICAvoFamNX93l2WWcMAAAAAElFTkSuQmCC";

/// The same conversation every time: a sentence, and a picture of a screen.
fn context(measured: bool) -> Vec<ContextItem> {
    let blob = Blob::new("image/png", SQUARE);
    // 1024x768, which this vendor cuts into two tiles across and two down:
    // 85 + 170 * 4 = 765 tokens
    let blob = match measured {
        true => blob.with_meta(json!({ "w": 1024, "h": 768 })),
        false => blob,
    };

    vec![
        ContextItem::user("Here is what the screen looks like. What is wrong with it?"),
        ContextItem::user(Content::Blob(Arc::new(blob))),
    ]
}

/// Counts one context with one counter and prints what it made of it.
fn measure(what: &str, counter: Arc<dyn TokenCounter>, measured: bool) {
    let kernel = Kernel::new(Config::default());
    kernel.set_counter(counter);
    kernel.push_all(context(measured));

    let budget = kernel.budget();
    println!("{what}");
    println!("  counter        {}", kernel.counter().name());
    println!("  context        {} tokens", budget.context_tokens);
    match budget.fully_counted() {
        true => println!("  unpriced       nothing; every piece of it has a number on it"),
        false => println!(
            "  unpriced       {} piece(s) - so the figure above is a floor",
            budget.uncounted
        ),
    }
    for item in kernel.items() {
        println!(
            "    [{}] {:<28} {:>5} tokens{}",
            item.id,
            item.content.to_text().chars().take(28).collect::<String>(),
            item.tokens,
            match item.uncounted {
                0 => String::new(),
                n => format!("  ({n} unpriced)"),
            }
        );
    }
    println!();
}

fn main() {
    println!("\n{}\n", "─".repeat(WIDTH));
    println!(
        "The same context, counted three ways. The picture is a 1024x768 screenshot;\n\
         the vendor being modelled charges 85 to look at one and 170 for each\n\
         512-pixel tile, so two tiles across and two down come to 765 tokens.\n"
    );
    println!("{}\n", "─".repeat(WIDTH));

    measure(
        "1. the counter the kernel starts with",
        Arc::new(BytesPerToken::default()),
        true,
    );
    measure(
        "2. a counter that knows the vendor's formula",
        Arc::new(Tiled::default()),
        true,
    );
    measure(
        "3. the same counter, handed a blob nobody measured",
        Arc::new(Tiled::default()),
        false,
    );

    println!("{}\n", "─".repeat(WIDTH));
    println!(
        "The first is honest and useless: it prices the prose, declines the picture,\n\
         and says so. The second is the whole point - the runtime learned no vendor's\n\
         arithmetic, the caller supplied it, and the budget is now a number worth\n\
         acting on. The third is the one to notice: a counter that knows a formula\n\
         still cannot apply it to a payload nobody measured, so it abstains exactly\n\
         as the default one does. That is the difference between a floor and a\n\
         fiction, and it is the reason `uncounted` is a count rather than a flag.\n"
    );
}
