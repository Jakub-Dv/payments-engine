use std::io::{self, Cursor, Read, Write};

use payments_engine::process;

mod common;

const INPUT: &[u8] = b"type,client,tx,amount\ndeposit,1,1,1.2345\n";

#[test]
fn columns_are_matched_by_header_instead_of_position() -> eyre::Result<()> {
    let mut output = Vec::new();
    process(
        &b"tx,amount,type,client\n1,1.2345,deposit,1\n"[..],
        &mut output,
    )?;
    common::assert_accounts(
        &output,
        "client,available,held,total,locked\n1,1.2345,0,1.2345,false\n",
    )
}

#[test]
fn invalid_headers_are_rejected_even_without_data_rows() {
    for input in [
        "type,client,tx\n",
        "type,client,tx,amount,amount\n",
        "kind,client,tx,amount\n",
        "type,client,client,amount\n",
    ] {
        let mut output = Vec::new();
        let result = process(input.as_bytes(), &mut output);
        assert!(result.is_err());
        assert!(output.is_empty());
    }
}

struct FailingReader {
    data: Cursor<Vec<u8>>,
}

impl Read for FailingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.data.read(buffer)? {
            0 => Err(io::Error::other("injected read failure")),
            count => Ok(count),
        }
    }
}

#[test]
fn read_errors_preserve_the_cause_and_never_write_partial_accounts() {
    for data in [Vec::new(), INPUT.to_vec()] {
        let mut output = Vec::new();
        let Err(error) = process(
            FailingReader {
                data: Cursor::new(data),
            },
            &mut output,
        ) else {
            panic!("reader failure should propagate");
        };
        assert!(format!("{error:#}").contains("injected read failure"));
        assert!(error.downcast_ref::<csv::Error>().is_some());
        assert!(output.is_empty());
    }
}

struct FailingWriter {
    fail_on_flush: bool,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_on_flush {
            Ok(bytes.len())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected write failure",
            ))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("injected flush failure"))
    }
}

#[test]
fn buffered_write_errors_preserve_the_io_cause() {
    let Err(error) = process(
        INPUT,
        FailingWriter {
            fail_on_flush: false,
        },
    ) else {
        panic!("write failure should propagate");
    };
    assert!(format!("{error:#}").contains("injected write failure"));
    let Some(cause) = error.downcast_ref::<io::Error>() else {
        panic!("I/O cause should remain available");
    };
    assert_eq!(cause.kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn final_flush_errors_are_propagated() {
    let Err(error) = process(
        INPUT,
        FailingWriter {
            fail_on_flush: true,
        },
    ) else {
        panic!("flush failure should propagate");
    };
    assert!(format!("{error:#}").contains("injected flush failure"));
    assert!(error.downcast_ref::<io::Error>().is_some());
}
