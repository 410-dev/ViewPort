//! `VP_NATURAL` collation: case-insensitive, with digit runs compared by numeric value
//! (`img2` < `img10`).

use std::cmp::Ordering;

pub const NATURAL: &str = "VP_NATURAL";

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a, b);
    loop {
        match (x.chars().next(), y.chars().next()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(cx), Some(cy)) if cx.is_ascii_digit() && cy.is_ascii_digit() => {
                let (nx, rx) = split_digits(x);
                let (ny, ry) = split_digits(y);
                let (tx, ty) = (nx.trim_start_matches('0'), ny.trim_start_matches('0'));
                let ord = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if ord != Ordering::Equal {
                    return ord;
                }
                (x, y) = (rx, ry);
            }
            (Some(cx), Some(cy)) => {
                let ord = cx.to_lowercase().cmp(cy.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                (x, y) = (&x[cx.len_utf8()..], &y[cy.len_utf8()..]);
            }
        }
    }
}

fn split_digits(s: &str) -> (&str, &str) {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    s.split_at(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_naturally() {
        let mut v = vec!["img10.jpg", "IMG2.jpg", "img1.jpg", "img02.jpg", "가.jpg", "b.jpg", "A.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["A.jpg", "b.jpg", "img1.jpg", "IMG2.jpg", "img02.jpg", "img10.jpg", "가.jpg"]);
    }
}
