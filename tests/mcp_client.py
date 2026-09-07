"""Tests use the same stdio transport as the shipped local CLI."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
# Execute under a distinct module name to avoid importing this compatibility module.
import importlib.util
_spec = importlib.util.spec_from_file_location("idea_db_stdio_client", Path(__file__).resolve().parents[1] / "scripts" / "mcp_client.py")
_module = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = _module
_spec.loader.exec_module(_module)
McpProtocolError = _module.McpProtocolError
McpToolError = _module.McpToolError
call_tool = _module.call_tool
