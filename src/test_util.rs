/// Assert that two Colors are approximately equal (per-channel tolerance).
///
/// Usage: `assert_color_eq!(actual, expected)` with default tolerance 1e-4,
/// or `assert_color_eq!(actual, expected, 1e-3)` with custom tolerance.
#[macro_export]
macro_rules! assert_color_eq {
    ($a:expr, $b:expr) => {
        assert_color_eq!($a, $b, 1e-4)
    };
    ($a:expr, $b:expr, $tol:expr) => {{
        let (a, b, tol) = (&$a, &$b, $tol);
        assert!(
            a.approx_eq(b, tol),
            "Color mismatch:\n  left:  ({:.5}, {:.5}, {:.5}, {:.5})\n  right: ({:.5}, {:.5}, {:.5}, {:.5})\n  tolerance: {}",
            a.r, a.g, a.b, a.a,
            b.r, b.g, b.b, b.a,
            tol,
        );
    }};
}
