use godot::classes::{INode2D, Node2D};
use godot::prelude::*;

/// Training-room backdrop: a light grid drawn in world space behind everything.
/// Static (the camera doesn't move), so it draws once. White comes from the clear color.
#[derive(GodotClass)]
#[class(base = Node2D)]
pub struct Grid {
    base: Base<Node2D>,
}

#[godot_api]
impl INode2D for Grid {
    fn init(base: Base<Node2D>) -> Self {
        Self { base }
    }

    fn ready(&mut self) {
        self.base_mut().queue_redraw();
    }

    fn draw(&mut self) {
        let (x0, x1) = (-400.0_f32, 1600.0_f32);
        let (y0, y1) = (-300.0_f32, 1200.0_f32);
        let step = 50.0_f32;
        let minor = Color::from_rgba(0.86, 0.86, 0.90, 1.0);
        let major = Color::from_rgba(0.74, 0.74, 0.80, 1.0); // every 5th line, heavier

        let mut base = self.base_mut();
        let mut i = 0;
        let mut x = x0;
        while x <= x1 {
            let c = if i % 5 == 0 { major } else { minor };
            base.draw_line(Vector2::new(x, y0), Vector2::new(x, y1), c);
            x += step;
            i += 1;
        }
        let mut j = 0;
        let mut y = y0;
        while y <= y1 {
            let c = if j % 5 == 0 { major } else { minor };
            base.draw_line(Vector2::new(x0, y), Vector2::new(x1, y), c);
            y += step;
            j += 1;
        }
    }
}
