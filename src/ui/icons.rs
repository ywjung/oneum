//! Vector icons drawn with the painter, so they stay crisp at any scale and do not depend
//! on which glyphs a font happens to contain.
use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2, pos2, vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Next,
    Previous,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    VolumeMuted,
    Queue,
    Info,
    Heart,
    HeartFilled,
    Search,
    Close,
    Plus,
    Back,
    Gear,
    Note,
    Warning,
    Album,
    Person,
    Clock,
    Playlist,
    Folder,
    Trash,
    Keyboard,
}

fn arrow_head(tip: Pos2, dir: Vec2, size: f32, color: Color32) -> Shape {
    let d = dir.normalized();
    let n = vec2(-d.y, d.x);
    Shape::convex_polygon(
        vec![
            tip,
            tip - d * size + n * size * 0.6,
            tip - d * size - n * size * 0.6,
        ],
        color,
        Stroke::NONE,
    )
}

/// Paints `icon` centred in `rect` (drawn inside a square of `rect`'s smaller side).
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let p = |x: f32, y: f32| pos2(c.x + (x - 0.5) * s, c.y + (y - 0.5) * s);
    let w = (s / 11.0).clamp(1.2, 2.2);
    let stroke = Stroke::new(w, color);
    let fill = |pts: Vec<Pos2>| Shape::convex_polygon(pts, color, Stroke::NONE);
    match icon {
        Icon::Play => {
            painter.add(fill(vec![p(0.28, 0.16), p(0.86, 0.5), p(0.28, 0.84)]));
        }
        Icon::Pause => {
            painter.rect_filled(Rect::from_min_max(p(0.24, 0.18), p(0.42, 0.82)), 1.5, color);
            painter.rect_filled(Rect::from_min_max(p(0.58, 0.18), p(0.76, 0.82)), 1.5, color);
        }
        Icon::Next => {
            painter.add(fill(vec![p(0.16, 0.22), p(0.62, 0.5), p(0.16, 0.78)]));
            painter.rect_filled(Rect::from_min_max(p(0.68, 0.22), p(0.80, 0.78)), 1.0, color);
        }
        Icon::Previous => {
            painter.add(fill(vec![p(0.84, 0.22), p(0.38, 0.5), p(0.84, 0.78)]));
            painter.rect_filled(Rect::from_min_max(p(0.20, 0.22), p(0.32, 0.78)), 1.0, color);
        }
        Icon::Shuffle => {
            // Two crossing paths ending in arrow heads on the right.
            let a = [p(0.12, 0.30), p(0.34, 0.30), p(0.62, 0.70), p(0.80, 0.70)];
            let b = [p(0.12, 0.70), p(0.34, 0.70), p(0.62, 0.30), p(0.80, 0.30)];
            painter.add(Shape::line(a.to_vec(), stroke));
            painter.add(Shape::line(b.to_vec(), stroke));
            painter.add(arrow_head(p(0.92, 0.70), vec2(1.0, 0.0), s * 0.16, color));
            painter.add(arrow_head(p(0.92, 0.30), vec2(1.0, 0.0), s * 0.16, color));
        }
        Icon::Repeat | Icon::RepeatOne => {
            // A loop: top edge going right, bottom edge going left.
            painter.add(Shape::line(
                vec![p(0.16, 0.50), p(0.16, 0.30), p(0.74, 0.30)],
                stroke,
            ));
            painter.add(arrow_head(p(0.88, 0.30), vec2(1.0, 0.0), s * 0.16, color));
            painter.add(Shape::line(
                vec![p(0.84, 0.50), p(0.84, 0.70), p(0.26, 0.70)],
                stroke,
            ));
            painter.add(arrow_head(p(0.12, 0.70), vec2(-1.0, 0.0), s * 0.16, color));
            if icon == Icon::RepeatOne {
                painter.add(Shape::line(
                    vec![p(0.45, 0.43), p(0.52, 0.38), p(0.52, 0.62)],
                    Stroke::new(w * 0.9, color),
                ));
            }
        }
        Icon::Volume | Icon::VolumeMuted => {
            painter.add(fill(vec![
                p(0.10, 0.38),
                p(0.26, 0.38),
                p(0.26, 0.62),
                p(0.10, 0.62),
            ]));
            painter.add(fill(vec![
                p(0.26, 0.38),
                p(0.48, 0.18),
                p(0.48, 0.82),
                p(0.26, 0.62),
            ]));
            if icon == Icon::Volume {
                for (r, span) in [(0.16, 0.9f32), (0.30, 0.8)] {
                    let pts: Vec<Pos2> = (0..=8)
                        .map(|i| {
                            let a = -span / 2.0 + span * i as f32 / 8.0;
                            p(0.50 + r * a.cos(), 0.5 + r * a.sin())
                        })
                        .collect();
                    painter.add(Shape::line(pts, stroke));
                }
            } else {
                painter.line_segment([p(0.60, 0.36), p(0.88, 0.64)], stroke);
                painter.line_segment([p(0.88, 0.36), p(0.60, 0.64)], stroke);
            }
        }
        Icon::Queue => {
            for y in [0.28, 0.50, 0.72] {
                painter.circle_filled(p(0.16, y), w * 0.9, color);
                painter.line_segment([p(0.30, y), p(0.88, y)], stroke);
            }
        }
        Icon::Info => {
            painter.circle_stroke(c, s * 0.38, stroke);
            painter.circle_filled(p(0.5, 0.31), w * 0.85, color);
            painter.line_segment([p(0.5, 0.45), p(0.5, 0.72)], stroke);
        }
        Icon::Heart | Icon::HeartFilled => {
            // Two lobes and a point. Built from convex pieces so it can be filled.
            let lobe = s * 0.215;
            let left = p(0.30, 0.36);
            let right = p(0.70, 0.36);
            let tip = p(0.5, 0.88);
            if icon == Icon::HeartFilled {
                painter.circle_filled(left, lobe, color);
                painter.circle_filled(right, lobe, color);
                painter.add(fill(vec![
                    p(0.105, 0.46),
                    p(0.5, 0.30),
                    p(0.895, 0.46),
                    tip,
                ]));
            } else {
                // Outline: upper arcs of both lobes joined to the tip.
                let mut pts: Vec<Pos2> = Vec::new();
                for i in 0..=10 {
                    let a = std::f32::consts::PI * (0.80 + 1.05 * i as f32 / 10.0);
                    pts.push(left + vec2(a.cos(), a.sin()) * lobe);
                }
                for i in 0..=10 {
                    let a = std::f32::consts::PI * (1.15 + 1.05 * i as f32 / 10.0);
                    pts.push(right + vec2(a.cos(), a.sin()) * lobe);
                }
                pts.push(tip);
                painter.add(Shape::closed_line(pts, stroke));
            }
        }
        Icon::Search => {
            painter.circle_stroke(p(0.43, 0.43), s * 0.25, stroke);
            painter.line_segment([p(0.62, 0.62), p(0.86, 0.86)], stroke);
        }
        Icon::Close => {
            painter.line_segment([p(0.24, 0.24), p(0.76, 0.76)], stroke);
            painter.line_segment([p(0.76, 0.24), p(0.24, 0.76)], stroke);
        }
        Icon::Plus => {
            painter.line_segment([p(0.5, 0.20), p(0.5, 0.80)], stroke);
            painter.line_segment([p(0.20, 0.5), p(0.80, 0.5)], stroke);
        }
        Icon::Back => {
            painter.add(Shape::line(
                vec![p(0.60, 0.20), p(0.30, 0.5), p(0.60, 0.80)],
                stroke,
            ));
        }
        Icon::Gear => {
            painter.circle_stroke(c, s * 0.22, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment(
                    [c + d * s * 0.30, c + d * s * 0.42],
                    Stroke::new(w * 1.4, color),
                );
            }
        }
        Icon::Note => {
            painter.circle_filled(p(0.34, 0.72), s * 0.13, color);
            painter.circle_filled(p(0.72, 0.64), s * 0.13, color);
            painter.line_segment([p(0.45, 0.72), p(0.45, 0.24)], stroke);
            painter.line_segment([p(0.83, 0.64), p(0.83, 0.16)], stroke);
            painter.line_segment([p(0.45, 0.24), p(0.83, 0.16)], Stroke::new(w * 1.6, color));
        }
        Icon::Warning => {
            painter.add(Shape::closed_line(
                vec![p(0.5, 0.14), p(0.90, 0.84), p(0.10, 0.84)],
                stroke,
            ));
            painter.line_segment([p(0.5, 0.40), p(0.5, 0.62)], stroke);
            painter.circle_filled(p(0.5, 0.73), w * 0.8, color);
        }
        Icon::Folder => {
            painter.add(Shape::closed_line(
                vec![
                    p(0.10, 0.24),
                    p(0.38, 0.24),
                    p(0.47, 0.35),
                    p(0.90, 0.35),
                    p(0.90, 0.80),
                    p(0.10, 0.80),
                ],
                stroke,
            ));
        }
        Icon::Keyboard => {
            painter.rect_stroke(
                Rect::from_min_max(p(0.08, 0.24), p(0.92, 0.76)),
                s * 0.10,
                stroke,
                egui::StrokeKind::Inside,
            );
            for x in [0.28, 0.50, 0.72] {
                painter.circle_filled(p(x, 0.42), w * 0.6, color);
            }
            painter.add(Shape::line(vec![p(0.32, 0.60), p(0.68, 0.60)], stroke));
        }
        Icon::Trash => {
            painter.add(Shape::line(vec![p(0.16, 0.28), p(0.84, 0.28)], stroke));
            painter.add(Shape::line(
                vec![p(0.40, 0.28), p(0.40, 0.16), p(0.60, 0.16), p(0.60, 0.28)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![p(0.25, 0.28), p(0.31, 0.86), p(0.69, 0.86), p(0.75, 0.28)],
                stroke,
            ));
        }
        Icon::Album => {
            painter.rect_stroke(
                Rect::from_min_max(p(0.12, 0.12), p(0.88, 0.88)),
                s * 0.14,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_stroke(c, s * 0.20, stroke);
            painter.circle_filled(c, w * 0.8, color);
        }
        Icon::Person => {
            painter.circle_stroke(p(0.5, 0.33), s * 0.17, stroke);
            let pts: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 12.0);
                    p(0.5 + 0.36 * a.cos(), 0.90 + 0.34 * a.sin())
                })
                .collect();
            painter.add(Shape::line(pts, stroke));
        }
        Icon::Clock => {
            painter.circle_stroke(c, s * 0.38, stroke);
            painter.add(Shape::line(
                vec![p(0.5, 0.28), p(0.5, 0.5), p(0.66, 0.60)],
                stroke,
            ));
        }
        Icon::Playlist => {
            for y in [0.26, 0.46] {
                painter.line_segment([p(0.12, y), p(0.80, y)], stroke);
            }
            painter.line_segment([p(0.12, 0.66), p(0.48, 0.66)], stroke);
            painter.circle_filled(p(0.66, 0.78), s * 0.10, color);
            painter.line_segment([p(0.75, 0.78), p(0.75, 0.52)], stroke);
            painter.line_segment([p(0.75, 0.52), p(0.90, 0.58)], stroke);
        }
    }
}
