use idea_db::{config::Config, mcp::IdeaDbMcp, neo4j::Neo4j};
use rmcp::{transport::async_rw::AsyncRwTransport, RoleServer, ServiceExt};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

const MAX_MCP_LINE_BYTES: usize =
    idea_db::limits::MAX_IMPORT_BYTES + idea_db::limits::MAX_BODY_BYTES;

struct BoundedLineReader<R> {
    inner: R,
    current_line: usize,
    max_line: usize,
}

impl<R> BoundedLineReader<R> {
    fn new(inner: R, max_line: usize) -> Self {
        Self {
            inner,
            current_line: 0,
            max_line,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BoundedLineReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut scratch = [0_u8; 8192];
        let capacity = scratch.len().min(buf.remaining());
        let mut temporary = ReadBuf::new(&mut scratch[..capacity]);
        match Pin::new(&mut self.inner).poll_read(cx, &mut temporary) {
            Poll::Ready(Ok(())) => {
                for byte in temporary.filled() {
                    if *byte == b'\n' {
                        self.current_line = 0;
                    } else {
                        self.current_line = self.current_line.saturating_add(1);
                        if self.current_line > self.max_line {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "MCP JSON line exceeds configured limit",
                            )));
                        }
                    }
                }
                buf.put_slice(temporary.filled());
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("idea-db-mcp: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let neo = Neo4j::new(&Config::from_env())?;
    let input = BoundedLineReader::new(tokio::io::stdin(), MAX_MCP_LINE_BYTES);
    let transport = AsyncRwTransport::<RoleServer, _, _>::new_server(input, tokio::io::stdout());
    let service = IdeaDbMcp::new(neo).serve(transport).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn bounded_reader_rejects_one_oversized_line() {
        let input = tokio::io::duplex(64);
        let (mut writer, reader) = input;
        let write = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            writer.write_all(b"123456789\n").await.unwrap();
        });
        let error = BoundedLineReader::new(reader, 8)
            .read_to_end(&mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        write.await.unwrap();
    }

    #[tokio::test]
    async fn bounded_reader_resets_at_newlines() {
        let input = tokio::io::duplex(64);
        let (mut writer, reader) = input;
        let write = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            writer.write_all(b"12345678\n12345678\n").await.unwrap();
        });
        let mut output = Vec::new();
        BoundedLineReader::new(reader, 8)
            .read_to_end(&mut output)
            .await
            .unwrap();
        assert_eq!(output, b"12345678\n12345678\n");
        write.await.unwrap();
    }
}
