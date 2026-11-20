//! Hit geometry for a Windows caption band without app controls, in logical points.
//! `trailing` includes disabled caption buttons: pressing one must not move the window.
pub(super) fn contains(width: f32, height: f32, trailing: f32, (x, y): (f32, f32)) -> bool {
    (0.0..height).contains(&y) && (0.0..width - trailing).contains(&x)
}