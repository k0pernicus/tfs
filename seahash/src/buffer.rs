use core::num::Wrapping as W;
use core::slice;

use diffuse;

/// Read a buffer smaller than 8 bytes into an integer in little-endian.
///
/// # Unsafety
///
/// This assumes that `buf.len() < 8`, and relies on that for memory safety.
#[inline(always)]
unsafe fn read_int(buf: &[u8]) -> W<u64> {
    let ptr = buf.as_ptr();
    // Break it down to reads of integers with widths in total spanning the buffer. This minimizes
    // the number of reads
    match buf.len() {
        // u8.
        1 => W(*ptr as u64),
        // u16.
        2 => W((*(ptr as *const u16)).to_le() as u64),
        // u16 + u8.
        3 => {
            let a = (*(ptr as *const u16)).to_le() as u64;
            let b = *ptr.offset(2) as u64;

            W(a | (b << 16))
        },
        // u32.
        4 => W((*(ptr as *const u32)).to_le() as u64),
        // u32 + u8.
        5 => {
            let a = (*(ptr as *const u32)).to_le() as u64;
            let b = *ptr.offset(4) as u64;

            W(a | (b << 32))
        },
        // u32 + u16.
        6 => {
            let a = (*(ptr as *const u32)).to_le() as u64;
            let b = (*(ptr.offset(4) as *const u16)).to_le() as u64;

            W(a | (b << 32))
        },
        // u32 + u16 + u8.
        7 => {
            let a = (*(ptr as *const u32)).to_le() as u64;
            let b = (*(ptr.offset(4) as *const u16)).to_le() as u64;
            let c = *ptr.offset(6) as u64;

            W(a | (b << 32) | (c << 48))
        },
        _ => W(0),
    }
}

/// Hash some buffer.
pub fn hash(buf: &[u8]) -> u64 {
    unsafe {
        // We fetch this in order to avoid aliasing things later on and thus breaking certain
        // optimizations.
        let len = buf.len() as isize;

        // We use 4 different registers to store seperate hash states, because this allows us to update
        // them seperately, and consequently exploiting ILP to update the states in parallel.
        let mut a = W(0x16f11fe89b0d677c);
        let mut b = W(0xb480a793d8e6c86c);
        let mut c = W(0x6fe2e5aaf078ebc9);
        // We mix `len` in to make sure the function is zero-sensitive in the excessive bytes.
        let mut d = W(0x14f994a4c5259381);

        // We pre-fetch the pointer to the buffer to avoid too many cache misses.
        let buf_ptr = buf.as_ptr();
        // The pointer to the current bytes.
        let mut ptr = buf_ptr as *const u64;
        /// The end of the "main segment", i.e. the biggest buffer s.t. the length is divisible by
        /// 32.
        let end_ptr = buf_ptr.offset(len & !0x1F) as *const u64;

        while ptr < end_ptr {
            // Read and diffuse the next 4 64-bit little-endian integers from their bytes. Note
            // that we on purpose not use `+=` and co., because it aliases the lvalue, making it
            // harder for LLVM to register allocate (it will have to inline the value behind the
            // pointer, effectively assuming that it is not aliased, which can be hard to prove).

            // Placing these updates inplace can have some negative consequences on especially
            // older architectures, where they can block ILP because they assume the evaluation of
            // the old `byte` is executed, which might trigger the diffusion to run serially.
            // However, not introducing a tmp register makes sure that you don't push from the
            // register to the stack, which comes with a performance hit.
            a = a + W((*ptr).to_le());
            ptr = ptr.offset(1);

            b = b + W((*ptr).to_le());
            ptr = ptr.offset(1);

            c = c + W((*ptr).to_le());
            ptr = ptr.offset(1);

            d = d + W((*ptr).to_le());
            ptr = ptr.offset(1);

            // Diffuse the updated registers. We hope that each of these are executed in parallel.
            a = diffuse(a);
            b = diffuse(b);
            c = diffuse(c);
            d = diffuse(d);
        }

        // Calculate the number of excessive bytes.
        let mut excessive = len as usize + buf_ptr as usize - end_ptr as usize;
        // Handle the excessive bytes.
        if excessive != 0 {
            if excessive >= 24 {
                // 24 bytes or more excessive.

                // Update `a`.
                a = a + W((*ptr).to_le());
                ptr = ptr.offset(1);
                // Update `b`.
                b = b + W((*ptr).to_le());
                ptr = ptr.offset(1);
                // Update `c`.
                c = c + W((*ptr).to_le());
                ptr = ptr.offset(1);

                // Diffuse `a`, `b`, and `c`.
                a = diffuse(a);
                b = diffuse(b);
                c = diffuse(c);

                // Decrease the excessive counter by the number of bytes read.
                excessive = excessive - 24;
            } else if excessive >= 16 {
                // 16 bytes or more excessive.

                // Update `a`.
                a = a + W((*ptr).to_le());
                ptr = ptr.offset(1);
                // Update `b`.
                b = b + W((*ptr).to_le());
                ptr = ptr.offset(1);

                // Diffuse `a` and `b`.
                a = diffuse(a);
                b = diffuse(b);

                // Decrease the excessive counter by the number of bytes read.
                excessive = excessive - 16;
            } else if excessive >= 8 {
                // 8 bytes or more excessive.

                // Update `a`.
                a = a + W((*ptr).to_le());
                ptr = ptr.offset(1);
                // Diffuse `a`.
                a = diffuse(a);

                // Decrease the excessive counter by the number of bytes read.
                excessive = excessive - 8;
            }

            if excessive != 0 {
                // If the number of excessive bytes is still non-zero, we read the rest (<8) bytes
                // and diffuse them into state `a`.
                a = a + read_int(slice::from_raw_parts(ptr as *const u8, excessive));
                a = diffuse(a);
            }
        }

        // Add the length in order to make the excessive bytes zero-sensitive.
        b = b + W(len as u64);

        // Diffuse `b` and `d` into `a` and `c`.
        a = a + diffuse(b);
        c = c + diffuse(d);

        // Diffuse `a` and `c`.
        (a + diffuse(c)).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use reference;

    fn hash_match(a: &[u8]) {
        assert_eq!(hash(a), reference::hash(a));
    }

    #[test]
    fn zero() {
        let arr = [0; 4096];
        for n in 0..4096 {
            hash_match(&arr[0..n]);
        }
    }

    #[test]
    fn seq() {
        let mut buf = [0; 4096];
        for i in 0..4096 {
            buf[i] = i as u8;
        }
        hash_match(&buf);
    }


    #[test]
    fn position_depedent() {
        let mut buf1 = [0; 4098];
        for i in 0..4098 {
            buf1[i] = i as u8;
        }
        let mut buf2 = [0; 4098];
        for i in 0..4098 {
            buf2[i] = i as u8 ^ 1;
        }

        assert!(hash(&buf1) != hash(&buf2));
    }

    #[test]
    fn shakespear() {
        hash_match(b"to be or not to be");
        hash_match(b"love is a wonderful terrible thing");
    }

    #[test]
    fn zero_senitive() {
        assert_ne!(hash(&[1, 2, 3, 4]), hash(&[1, 0, 2, 3, 4]));
        assert_ne!(hash(&[1, 2, 3, 4]), hash(&[1, 0, 0, 2, 3, 4]));
        assert_ne!(hash(&[1, 2, 3, 4]), hash(&[1, 2, 3, 4, 0]));
        assert_ne!(hash(&[1, 2, 3, 4]), hash(&[0, 1, 2, 3, 4]));
        assert_ne!(hash(&[0, 0, 0]), hash(&[0, 0, 0, 0, 0]));
    }

    #[test]
    fn not_equal() {
        assert_ne!(hash(b"to be or not to be "), hash(b"to be or not to be"));
        assert_ne!(hash(b"jkjke"), hash(b"jkjk"));
        assert_ne!(hash(b"ijkjke"), hash(b"ijkjk"));
        assert_ne!(hash(b"iijkjke"), hash(b"iijkjk"));
        assert_ne!(hash(b"iiijkjke"), hash(b"iiijkjk"));
        assert_ne!(hash(b"iiiijkjke"), hash(b"iiiijkjk"));
        assert_ne!(hash(b"iiiiijkjke"), hash(b"iiiiijkjk"));
        assert_ne!(hash(b"iiiiiijkjke"), hash(b"iiiiiijkjk"));
        assert_ne!(hash(b"iiiiiiijkjke"), hash(b"iiiiiiijkjk"));
        assert_ne!(hash(b"iiiiiiiijkjke"), hash(b"iiiiiiiijkjk"));
        assert_ne!(hash(b"ab"), hash(b"bb"));
    }
}