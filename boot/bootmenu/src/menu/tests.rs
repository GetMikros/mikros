use super::*;
use alloc::string::ToString;
use alloc::vec;

const W: i32 = 1920;
const H: i32 = 1080;

fn card(name: &str, detail: Option<&str>, color: u32) -> Card {
    Card {
        name: name.to_string(),
        detail: detail.map(str::to_string),
        color: Color(color),
        icon: None,
    }
}

fn menu() -> Menu {
    Menu {
        cards: vec![
            card("Alpha", Some("First"), 0x3b82f6),
            card("Beta", None, 0xf59e0b),
            card("Gamma", Some("Third"), 0x10b981),
        ],
        buttons: vec![Power::Restart, Power::ShutDown, Power::FirmwareSettings],
        notice: None,
        focus: Some((Item::Card(0), FocusSource::Keyboard)),
        countdown: None,
    }
}

fn render(menu: &Menu) -> Canvas {
    let mut canvas = Canvas::new(W as usize, H as usize);
    menu.draw(&mut Text::new().unwrap(), &mut canvas);
    canvas
}

/// Whether any pixel in the rectangle differs from the background.
fn lit(canvas: &Canvas, (x, y, w, h): Rect) -> bool {
    (y..y + h).any(|py| (x..x + w).any(|px| canvas.get(px as usize, py as usize) != BACKGROUND))
}

#[test]
fn orders_items_cards_then_buttons() {
    let m = menu();
    assert_eq!(
        m.items(),
        [
            Item::Card(0),
            Item::Card(1),
            Item::Card(2),
            Item::Button(Power::Restart),
            Item::Button(Power::ShutDown),
            Item::Button(Power::FirmwareSettings),
        ]
    );
}

#[test]
fn lays_out_rows_around_the_center() {
    let m = menu();
    assert_eq!(m.bounds(Item::Card(0), W, H), (691, 419, 162, 162));
    assert_eq!(m.bounds(Item::Card(1), W, H), (879, 419, 162, 162));
    assert_eq!(m.bounds(Item::Card(2), W, H), (1067, 419, 162, 162));
    assert_eq!(
        m.bounds(Item::Button(Power::Restart), W, H),
        (844, 646, 56, 56)
    );
    assert_eq!(
        m.bounds(Item::Button(Power::FirmwareSettings), W, H),
        (1020, 646, 56, 56)
    );

    let mut two = menu();
    two.buttons.pop();
    assert_eq!(
        two.bounds(Item::Button(Power::Restart), W, H),
        (888, 646, 56, 56)
    );
}

#[test]
fn keeps_sizes_at_other_resolutions() {
    let m = menu();
    assert_eq!(m.bounds(Item::Card(1), 1280, 720), (559, 239, 162, 162));
}

#[test]
fn finds_items_under_the_pointer() {
    let m = menu();
    assert_eq!(m.item_at(772, 500, W, H), Some(Item::Card(0)));
    assert_eq!(
        m.item_at(960, 674, W, H),
        Some(Item::Button(Power::ShutDown))
    );
    assert_eq!(m.item_at(866, 500, W, H), None, "between cards");
    assert_eq!(m.item_at(10, 10, W, H), None);
}

#[test]
fn draws_keyboard_focus_in_the_accent_color() {
    let canvas = render(&menu());
    assert_eq!(canvas.get(691, 500), ACCENT, "focus ring of the first card");
    assert_eq!(
        canvas.get(879, 500),
        BACKGROUND,
        "no ring on the second card"
    );
    assert_eq!(
        canvas.get(772, 477),
        Color(0x3b82f6),
        "tile of the first card"
    );
    assert!(
        lit(&canvas, (700, 590, 140, 16)),
        "press to boot under the focused card"
    );
    assert!(
        !lit(&canvas, (888, 590, 140, 16)),
        "nothing under the second card"
    );
    assert!(lit(&canvas, (800, 355, 320, 22)), "heading");
    assert!(lit(&canvas, (800, 1048, 320, 16)), "hint at the bottom");
}

#[test]
fn draws_pointer_focus_with_a_subtle_outline() {
    let mut m = menu();
    m.focus = Some((Item::Card(1), FocusSource::Pointer));
    let canvas = render(&m);
    assert_eq!(canvas.get(691, 500), BACKGROUND);
    assert_eq!(canvas.get(878, 500), SUBTLE_OUTLINE);
    assert!(lit(&canvas, (888, 590, 140, 16)));
}

#[test]
fn labels_a_focused_button() {
    let mut m = menu();
    m.focus = Some((Item::Button(Power::ShutDown), FocusSource::Keyboard));
    let canvas = render(&m);
    assert_eq!(canvas.get(986, 674), ACCENT, "ring of the focused button");
    assert_eq!(canvas.get(960, 690), SURFACE_FOCUSED);
    assert_eq!(canvas.get(872, 690), SURFACE);
    assert!(lit(&canvas, (900, 712, 120, 14)), "label under the row");
}

#[test]
fn draws_notices_and_countdowns() {
    let mut m = menu();
    assert!(!lit(&render(&m), (0, 38, W, 20)));
    m.notice = Some("Something went wrong".to_string());
    m.countdown = Some(("Alpha".to_string(), 5));
    let canvas = render(&m);
    assert!(lit(&canvas, (0, 38, W, 20)), "notice near the top");
    let warning = (0..W).any(|x| (38..58).any(|y| canvas.get(x as usize, y as usize) == WARNING));
    assert!(warning);
}

#[test]
fn draws_the_empty_card_with_the_notice() {
    let m = Menu {
        cards: Vec::new(),
        buttons: vec![Power::Restart, Power::ShutDown],
        notice: Some("line 3: `id` is empty".to_string()),
        focus: None,
        countdown: None,
    };
    let canvas = render(&m);
    assert_eq!(canvas.get(960, 450), SURFACE, "inside the card");
    assert_eq!(canvas.get(680, 500), BORDER, "card border");
    assert!(lit(&canvas, (800, 515, 320, 20)), "notice inside the card");
    assert!(!lit(&canvas, (0, 38, W, 20)), "no notice at the top");
}

#[test]
fn draws_icons_in_place_of_the_tile() {
    let mut m = menu();
    m.cards[0].icon = Some(Image {
        width: 72,
        height: 36,
        pixels: vec![0xffff0000; 72 * 36],
    });
    let canvas = render(&m);
    let (x, y, w, _) = m.bounds(Item::Card(0), W, H);
    let (left, top) = ((x + w / 2 - 36) as usize, (y + 22) as usize);
    assert_eq!(
        canvas.get(left + 36, top + 36),
        Color(0xff0000),
        "icon center"
    );
    assert_eq!(
        canvas.get(left + 36, top + 30),
        Color(0xff0000),
        "no letter"
    );
    assert_eq!(
        canvas.get(left + 36, top + 10),
        BACKGROUND,
        "above the icon"
    );
    assert_eq!(canvas.get(left, top + 18), BACKGROUND, "rounded corner");
    assert_eq!(canvas.get(left + 1, top + 36), Color(0xff0000), "left edge");
}
