use std::mem;
use std::pin::Pin;
use std::string::FromUtf8Error;
use std::task::{Context, Poll};

use futures_util::ready;
use pin_project_lite::pin_project;
use tokio::io::{self, AsyncBufRead};

use tokio_ as tokio;

pin_project! {
    #[derive(Debug)]
    #[must_use = "streams do nothing unless polled"]
    pub struct Lines<R> {
        #[pin]
        reader: R,
        buf: String,
        bytes: Vec<u8>,
        read: usize,
    }
}

pub(crate) fn lines<R>(reader: R) -> Lines<R>
where
    R: AsyncBufRead,
{
    Lines {
        reader,
        buf: String::new(),
        bytes: Vec::new(),
        read: 0,
    }
}

impl<R> Lines<R>
where
    R: AsyncBufRead,
{
    /// Polls for the next line in the stream.
    ///
    /// This method returns:
    ///
    ///  * `Poll::Pending` if the next line is not yet available.
    ///  * `Poll::Ready(Ok(Some(line)))` if the next line is available.
    ///  * `Poll::Ready(Ok(None))` if there are no more lines in this stream.
    ///  * `Poll::Ready(Err(err))` if an IO error occurred while reading the next line.
    ///
    /// When the method returns `Poll::Pending`, the `Waker` in the provided
    /// `Context` is scheduled to receive a wakeup when more bytes become
    /// available on the underlying IO resource.  Note that on multiple calls to
    /// `poll_next_line`, only the `Waker` from the `Context` passed to the most
    /// recent call is scheduled to receive a wakeup.
    pub fn poll_next_line(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<Option<String>>> {
        let me = self.project();

        let n = ready!(read_line(me.reader, cx, me.buf, me.bytes, me.read))?;
        debug_assert_eq!(*me.read, 0);

        if n == 0 && me.buf.is_empty() {
            return Poll::Ready(Ok(None));
        }

        if me.buf.ends_with('\n') {
            me.buf.pop();

            if me.buf.ends_with('\r') {
                me.buf.pop();
            }
        }
        }

        Poll::Ready(Ok(Some(mem::take(me.buf))))
    }
}

fn read_line<R: AsyncBufRead + ?Sized>(
    reader: Pin<&mut R>,
    cx: &mut Context<'_>,
    output: &mut String,
    buf: &mut Vec<u8>,
    read: &mut usize,
) -> Poll<io::Result<usize>> {
    let io_res = ready!(read_until(reader, cx, b'\n', buf, read));
    let utf8_res = String::from_utf8(mem::take(buf));

    // At this point both buf and output are empty. The allocation is in utf8_res.

    debug_assert!(buf.is_empty());
    debug_assert!(output.is_empty());
    finish_string_read(io_res, utf8_res, *read, output, false)
}

fn read_until<R: AsyncBufRead + ?Sized>(
    mut reader: Pin<&mut R>,
    cx: &mut Context<'_>,
    delimiter: u8,
    buf: &mut Vec<u8>,
    read: &mut usize,
) -> Poll<io::Result<usize>> {
    loop {
        let (done, used) = {
            let available = ready!(reader.as_mut().poll_fill_buf(cx))?;
            if let Some(i) = memchr::memchr(delimiter, available) {
                buf.extend_from_slice(&available[..=i]);
                (true, i + 1)
            } else {
                buf.extend_from_slice(available);
                (false, available.len())
            }
        };
        reader.as_mut().consume(used);
        *read += used;
        if done || used == 0 {
            return Poll::Ready(Ok(mem::replace(read, 0)));
        }
    }
}

fn finish_string_read(
    io_res: io::Result<usize>,
    utf8_res: Result<String, FromUtf8Error>,
    read: usize,
    output: &mut String,
    truncate_on_io_error: bool,
) -> Poll<io::Result<usize>> {
    match (io_res, utf8_res) {
        (Ok(num_bytes), Ok(string)) => {
            debug_assert_eq!(read, 0);
            *output = string;
            Poll::Ready(Ok(num_bytes))
        }
        (Err(io_err), Ok(string)) => {
            *output = string;
            if truncate_on_io_error {
                let original_len = output.len() - read;
                output.truncate(original_len);
            }
            Poll::Ready(Err(io_err))
        }
        (Ok(num_bytes), Err(utf8_err)) => {
            debug_assert_eq!(read, 0);
            put_back_original_data(output, utf8_err.into_bytes(), num_bytes);

            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )))
        }
        (Err(io_err), Err(utf8_err)) => {
            put_back_original_data(output, utf8_err.into_bytes(), read);

            Poll::Ready(Err(io_err))
        }
    }
}

fn put_back_original_data(output: &mut String, mut vector: Vec<u8>, num_bytes_read: usize) {
    let original_len = vector.len() - num_bytes_read;
    vector.truncate(original_len);
    *output = String::from_utf8(vector).expect("The original data must be valid utf-8.");
}
