//! A small editable adaptation of WordCraft's studio handbook sample.
//!
//! Text adapted from WordCraft `crates/engine/src/sample.rs`, revision
//! 3e2c79407d9546474afd84619099f06849aa1ece, MIT licensed. See
//! `third_party/wordcraft/LICENSE-MIT`. Features not represented by the
//! Holonomy document model are intentionally expressed as plain text.
use holonomy_text::{Editor, EditorError, STYLE_BOLD, STYLE_HEADER, STYLE_ITALIC};

pub const TITLE: &str = "The Open Studio Handbook";

const TEXT: &str = concat!(
    "The Open Studio Handbook\n",
    "How a small community of makers shares space, tools and time\n\n",
    "Every Thursday evening the old print shop on Alder Street fills with people who make things. Painters set up by the north windows, letterpress regulars claim the back room, and somebody always brings bread. This handbook collects what we have learned about running a shared studio: the routines that keep it pleasant, the tools we pool, and the small agreements that let thirty strangers become a crew.\n\n",
    "Why share a studio?\n",
    "Rent is the obvious reason, but it is not the best one. A shared room puts your work next to work you would never have seen otherwise. A ceramicist explains glaze chemistry to a comic artist; a photographer borrows a weaver's loom for a still life. The cross-talk is the point.\n\n",
    "A studio is a place where unfinished things are welcome.\n\n",
    "What we agree on\n",
    "- Clean as you go. Leave a table the way you would like to find it.\n",
    "- Label what is yours. Unlabelled supplies on the commons shelf are fair game.\n",
    "- Ask before you borrow. Even from the shelf, a quick note helps everyone keep track.\n",
    "- Quiet hours run from 9 to 11 on weekday mornings.\n\n",
    "Shared equipment\n",
    "The list below shows what the membership owns together and who keeps each item in working order.\n",
    "Etching press - Back room - Mara O. - Sign-up sheet\n",
    "Kiln (cone 10) - Yard shed - Theo P. - 48 h notice\n",
    "Large-format printer - Office - Ines R. - Calendar\n",
    "Light table - North windows - Anyone - First come\n\n",
    "Opening the studio\n",
    "1. Switch on the main breaker by the door.\n",
    "2. Open both skylights unless it is raining.\n",
    "3. Start the kettle. This step is not optional.\n",
    "4. Check the booking sheet for the kiln and press.\n\n",
    "Membership\n",
    "Membership is pay what you can, reviewed every season. New members shadow a regular for their first two visits, and everyone takes one cleaning shift a month. If you have never made anything before, you are exactly who we hoped would show up.\n",
    "Questions go to the front desk, or to the notice board by the sink.\n",
);

/// Build a fresh independent document. Styles use the existing persisted span format.
pub fn studio_handbook() -> Result<Editor, EditorError> {
    let mut editor = Editor::from_text(TEXT.as_bytes())?;
    for heading in [
        TITLE,
        "Why share a studio?",
        "What we agree on",
        "Shared equipment",
        "Opening the studio",
        "Membership",
    ] {
        let start = TEXT.find(heading).expect("sample heading is present") as u32;
        editor.style_range(
            start,
            start + heading.len() as u32,
            STYLE_HEADER | STYLE_BOLD,
            0x001A_5C75,
        )?;
    }
    for text in [
        "Painters",
        "Clean as you go.",
        "Label what is yours.",
        "Ask before you borrow.",
        "Quiet hours",
        "pay what you can",
    ] {
        let start = TEXT.find(text).expect("sample bold text is present") as u32;
        editor.style_range(start, start + text.len() as u32, STYLE_BOLD, 0)?;
    }
    for text in [
        "letterpress",
        "A studio is a place where unfinished things are welcome.",
    ] {
        let start = TEXT.find(text).expect("sample italic text is present") as u32;
        editor.style_range(start, start + text.len() as u32, STYLE_ITALIC, 0)?;
    }
    editor.caret_to(0)?;
    Ok(editor)
}
