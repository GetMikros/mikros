//! Reading a `.curpack` file front to back, as the module documentation of
//! the crate lays it out, checking every count against the bytes left
//! before anything is allocated.

use crate::*;

impl Pack {
    pub fn parse(bytes: &[u8]) -> Result<Pack, Error> {
        let mut r = Reader { bytes, at: 0 };
        if r.take(MAGIC.len())? != MAGIC {
            return Err(Error::BadMagic);
        }
        let version = r.u16()?;
        if version != VERSION {
            return Err(Error::UnknownVersion(version));
        }
        let size = r.u16()?;
        let count = usize::from(r.u16()?);
        if size == 0 || count > MAX_CURSORS {
            return Err(Error::Invalid("the size or the number of cursors"));
        }
        let cursors = (0..count).map(|_| r.cursor()).collect::<Result<_, _>>()?;
        if r.at != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        let pack = Pack { size, cursors };
        pack.check()?;
        Ok(pack)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Truncated)?;
        let taken = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn point(&mut self) -> Result<(f32, f32), Error> {
        Ok((self.f32()?, self.f32()?))
    }

    fn colour(&mut self) -> Result<[u8; 4], Error> {
        Ok(self.take(4)?.try_into().expect("4 bytes"))
    }

    /// `count` items of at least `each` bytes, each read by `item`, with
    /// room only for those that the rest of the file can hold.
    fn many<T>(
        &mut self,
        count: usize,
        each: usize,
        mut item: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        if count.saturating_mul(each) > self.bytes.len() - self.at {
            return Err(Error::Truncated);
        }
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(item(self)?);
        }
        Ok(items)
    }

    fn cursor(&mut self) -> Result<Cursor, Error> {
        let len = usize::from(self.u8()?);
        let name = std::str::from_utf8(self.take(len)?)
            .map_err(|_| Error::Invalid("a cursor's name"))?
            .to_owned();
        let hotspot = self.point()?;
        let frames = usize::from(self.u16()?);
        let frames = self.many(frames, 6, Reader::frame)?;
        Ok(Cursor {
            name,
            hotspot,
            frames,
        })
    }

    fn frame(&mut self) -> Result<Frame, Error> {
        let duration_ms = self.u32()?;
        let paths = usize::from(self.u16()?);
        let paths = self.many(paths, 9, Reader::path)?;
        Ok(Frame { duration_ms, paths })
    }

    fn path(&mut self) -> Result<Path, Error> {
        let what = self.u8()?;
        if what & !3 != 0 {
            return Err(Error::Invalid("a path's kind"));
        }
        let fill = match what & 1 {
            0 => None,
            _ => Some(Fill {
                rule: match self.u8()? {
                    0 => FillRule::NonZero,
                    1 => FillRule::EvenOdd,
                    _ => return Err(Error::Invalid("a fill rule")),
                },
                paint: self.paint()?,
            }),
        };
        let stroke = match what & 2 {
            0 => None,
            _ => Some(Stroke {
                width: self.f32()?,
                join: match self.u8()? {
                    0 => Join::Miter,
                    1 => Join::Round,
                    2 => Join::Bevel,
                    _ => return Err(Error::Invalid("a stroke's join")),
                },
                cap: match self.u8()? {
                    0 => Cap::Butt,
                    1 => Cap::Round,
                    2 => Cap::Square,
                    _ => return Err(Error::Invalid("a stroke's cap")),
                },
                miter_limit: self.f32()?,
                paint: self.paint()?,
            }),
        };
        let verbs = self.u32()? as usize;
        let verbs = self.many(verbs, 1, |r| {
            Ok(match r.u8()? {
                0 => Verb::Move,
                1 => Verb::Line,
                2 => Verb::Quad,
                3 => Verb::Cubic,
                4 => Verb::Close,
                _ => return Err(Error::Invalid("a path's verb")),
            })
        })?;
        let points = self.u32()? as usize;
        let points = self.many(points, 8, Reader::point)?;
        Ok(Path {
            verbs,
            points,
            fill,
            stroke,
        })
    }

    fn paint(&mut self) -> Result<Paint, Error> {
        let kind = self.u8()?;
        if kind == 0 {
            return Ok(Paint::Solid(self.colour()?));
        }
        let paint = match kind {
            1 => Paint::Linear {
                start: self.point()?,
                end: self.point()?,
                gradient: self.gradient()?,
            },
            2 => Paint::Radial {
                centre: self.point()?,
                radius: self.f32()?,
                focal: self.point()?,
                gradient: self.gradient()?,
            },
            _ => return Err(Error::Invalid("a paint's kind")),
        };
        Ok(paint)
    }

    fn gradient(&mut self) -> Result<Gradient, Error> {
        let spread = match self.u8()? {
            0 => Spread::Pad,
            1 => Spread::Reflect,
            2 => Spread::Repeat,
            _ => return Err(Error::Invalid("a gradient's spread")),
        };
        let mut transform = [0.0; 6];
        for value in &mut transform {
            *value = self.f32()?;
        }
        let stops = usize::from(self.u8()?);
        let stops = self.many(stops, 8, |r| {
            Ok(Stop {
                offset: r.f32()?,
                colour: r.colour()?,
            })
        })?;
        Ok(Gradient {
            stops,
            spread,
            transform,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::pack;

    #[test]
    fn reads_what_it_writes() {
        let pack = pack();
        let bytes = pack.to_bytes().unwrap();
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(Pack::parse(&bytes), Ok(pack.clone()));
        assert_eq!(pack.cursor_or_default("wait").name, "wait");
        assert_eq!(pack.cursor_or_default("text").name, "default");
    }

    #[test]
    fn refuses_every_truncation_and_trailing_bytes() {
        let bytes = pack().to_bytes().unwrap();
        for end in 0..bytes.len() {
            assert!(Pack::parse(&bytes[..end]).is_err(), "{end} bytes");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(Pack::parse(&longer), Err(Error::TrailingBytes));
    }

    #[test]
    fn checks_the_magic_and_the_version() {
        let mut bytes = pack().to_bytes().unwrap();
        bytes[8] = 2;
        assert_eq!(Pack::parse(&bytes), Err(Error::UnknownVersion(2)));
        bytes[0] = b'X';
        assert_eq!(Pack::parse(&bytes), Err(Error::BadMagic));
    }

    #[test]
    fn refuses_unknown_values_in_the_bytes() {
        let bytes = pack().to_bytes().unwrap();
        // The first path's kind, after the header, the name "default",
        // the hotspot, the frame count, the duration and the path count.
        let what = 8 + 6 + 1 + 7 + 8 + 2 + 4 + 2;
        assert_eq!(bytes[what], 3);
        let mut changed = bytes.clone();
        changed[what] = 4;
        assert_eq!(Pack::parse(&changed), Err(Error::Invalid("a path's kind")));
        let mut changed = bytes;
        changed[what + 1] = 9;
        assert_eq!(Pack::parse(&changed), Err(Error::Invalid("a fill rule")));
    }
}
