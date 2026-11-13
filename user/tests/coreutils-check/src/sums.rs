//! The utilities that sum files: each sums `a.txt`, whose sums are those
//! that GNU coreutils gives, and fails on a missing file.

use crate::Checks;

/// The utilities and what each writes for `a.txt`.
const SUMS: [(&str, &str); 9] = [
    ("md5sum", "deed54b823522e0525693b090363f9df  a.txt\n"),
    (
        "sha1sum",
        "98ce56098daf1a2ffe03a0d108ea841f1e4e6c69  a.txt\n",
    ),
    (
        "sha224sum",
        "53eb9a854bdcc414d3fc4c8a3e730b3a8083225956e3b97f71305921  a.txt\n",
    ),
    (
        "sha256sum",
        "b6285c57e8797db5d4c51c80d6f11938afda9b11c6a003549709189e9b4b92a2  a.txt\n",
    ),
    (
        "sha384sum",
        "07f35c1352d98d903358c643f2da91a50c64730049616e63d2b530b7c1069ca7\
         480da924002a7bb260864aaa52833e0e  a.txt\n",
    ),
    (
        "sha512sum",
        "4379c7e10619d0e4867e9c18a140a1420c8cb3510e040e2930651c1b1aabb0a6\
         68ba9d234875d98fd0d01f5486dd03079c99bc80440f76d1d7c6adf6356b299c  a.txt\n",
    ),
    (
        "b2sum",
        "dc95ce6fe6b573b607deda3d7cad9d54ac650f9522319b137e22c682aed19349\
         28ecc62f4f4d039b9d6be1f867dc1f1ba4e69888e7564c9f2ec321b1091beb1a  a.txt\n",
    ),
    ("cksum", "3358846701 14 a.txt\n"),
    ("sum", "61671     1 a.txt\n"),
];

pub fn check(c: &mut Checks) {
    for (utility, sum) in SUMS {
        c.ok(&[utility, "a.txt"], "", sum);
        c.err(&[utility, "missing"], 1);
    }
}
