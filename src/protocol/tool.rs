//! Read-only tool DTOs verified against Agent 0617433 `tool_data.rs` and rpc.md.
use super::*;

impl fmt::Debug for ToolInvocationWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolInvocation")
            .field("tool_ref", &self.tool_ref)
            .field("input_bytes", &self.input.total_bytes)
            .finish()
    }
}
impl fmt::Debug for ToolSubjectWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::File { .. } => "File",
            Self::Command { .. } => "Command",
            Self::Other => "Other",
        })
    }
}
impl fmt::Debug for ToolInputSummaryWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolInputSummary")
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}
impl fmt::Debug for ToolProcessChunkWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolProcessChunk")
            .field("stream", &self.stream)
            .field("base_offset", &self.base_offset)
            .field("next_offset", &self.next_offset)
            .field("data_bytes", &self.data.len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ToolReadResult {
    pub invocation: Option<ToolInvocationWire>,
    pub execution: ToolExecutionWire,
    #[serde(default)]
    pub display: Option<ToolDisplayWire>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct ToolOutputPage {
    pub tool_ref: ToolRefWire,
    pub stream: ToolDataStreamWire,
    pub encoding: String,
    pub base_offset: u64,
    pub next_offset: u64,
    pub observed_end: u64,
    pub eof: bool,
    pub truncated: bool,
    pub availability: ToolDataAvailabilityWire,
    pub data: String,
}

impl fmt::Debug for ToolOutputPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolOutputPage")
            .field("tool_ref", &self.tool_ref)
            .field("stream", &self.stream)
            .field("base_offset", &self.base_offset)
            .field("next_offset", &self.next_offset)
            .field("eof", &self.eof)
            .field("availability", &self.availability)
            .field("data_bytes", &self.data.len())
            .finish()
    }
}

#[derive(Serialize)]
struct ReadParams<'a> {
    #[serde(flatten)]
    tool_ref: &'a ToolRefWire,
    max_bytes: usize,
}
#[derive(Serialize)]
struct OutputParams<'a> {
    #[serde(flatten)]
    tool_ref: &'a ToolRefWire,
    stream: ToolDataStreamWire,
    offset: u64,
    max_bytes: usize,
}
impl OutgoingRequest {
    pub fn tool_read(id: RequestId, tool_ref: &ToolRefWire) -> Self {
        Self::new(
            id,
            METHOD_TOOL_READ,
            serde_json::to_value(ReadParams {
                tool_ref,
                max_bytes: 262_144,
            })
            .expect("tool read params"),
        )
    }
    pub fn tool_read_display(id: RequestId, tool_ref: &ToolRefWire) -> Self {
        let mut request = Self::tool_read(id, tool_ref);
        request.params["display"] = serde_json::Value::Bool(true);
        request
    }
    pub fn tool_output(
        id: RequestId,
        tool_ref: &ToolRefWire,
        stream: ToolDataStreamWire,
        offset: u64,
    ) -> Self {
        Self::new(
            id,
            METHOD_TOOL_OUTPUT,
            serde_json::to_value(OutputParams {
                tool_ref,
                stream,
                offset,
                max_bytes: crate::limits::TOOL_PAGE_BYTES,
            })
            .expect("tool output params"),
        )
    }
}
impl RpcResponse {
    pub fn parse_tool_read(&self) -> Result<ToolReadResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_tool_output(&self) -> Result<ToolOutputPage, RpcResponseError> {
        self.result_as()
    }
}
impl ToolDataStreamWire {
    pub fn label(self) -> &'static str {
        match self {
            Self::Input => "输入",
            Self::Output => "结果",
            Self::Stdout => "标准输出",
            Self::Stderr => "标准错误",
        }
    }
    pub fn index(self) -> usize {
        match self {
            Self::Input => 0,
            Self::Output => 1,
            Self::Stdout => 2,
            Self::Stderr => 3,
        }
    }
}
impl ToolExecutionStateWire {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Denied | Self::Cancelled | Self::InputProvided
        )
    }
}
