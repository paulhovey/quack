//! `view_image`: an image file as vision input, alongside a caption in the tool result.

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use serde_json::{Value, json};

use crate::paths;
use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str};

const PATH: &[&str] = &["path", "file_path", "filename", "file"];

#[derive(Debug)]
pub struct ViewImage;

impl Tool for ViewImage {
    fn name(&self) -> &str {
        "view_image"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        "View an image file (png, jpeg, gif, webp): it is sent to you as an image, so you \
         see it instead of reading bytes as text. Use this instead of `read` for images. \
         Images over 3 MB are refused; downscale them first."
            .to_owned()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the image file" }
            },
            "required": ["path"]
        })
    }

    fn subject(&self, args: &Value, ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Read(paths::resolve(
            &ctx.root,
            arg_str(args, PATH)?,
        )))
    }

    fn summary(&self, args: &Value, ctx: &ToolCtx) -> String {
        let path = arg_str(args, PATH)
            .map(|p| paths::display(&ctx.root, &paths::resolve(&ctx.root, p)))
            .unwrap_or_default();
        format!("view_image {path}")
    }

    fn preview(&self, args: &Value, ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: self.summary(args, ctx),
            arguments: args.to_string(),
        })
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move { view(&args, ctx).unwrap_or_else(ToolOutput::error) })
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        ctx.vision
    }
}

fn view(args: &Value, ctx: &ToolCtx) -> Result<ToolOutput, String> {
    let path = paths::resolve(&ctx.root, arg_str(args, PATH)?);
    let shown = paths::display(&ctx.root, &path);
    let bytes = std::fs::read(&path).map_err(|e| crate::text::io_msg(&path, &e))?;
    let image = quack_protocol::Image::from_bytes(&bytes).map_err(|e| format!("{shown}: {e}"))?;
    ctx.reads.record(&path);
    let size = if bytes.len() >= 1024 * 1024 {
        format!("{:.1}MB", bytes.len() as f64 / (1024.0 * 1024.0))
    } else {
        format!("{}KB", bytes.len().div_ceil(1024))
    };
    let dims = match (image.width, image.height) {
        (Some(w), Some(h)) => format!(" {w}x{h}"),
        _ => String::new(),
    };
    let mut out = ToolOutput::ok(
        format!("[image: {shown} {size}{dims}]"),
        format!("image{dims}"),
    );
    out.images = vec![image];
    Ok(out)
}
