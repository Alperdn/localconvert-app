import type { Capability } from "../../store/useStore";
import { apiRequest } from "./client";

interface CapabilitiesResponse {
  capabilities: Capability[];
  limits: { max_upload_bytes: number; session_quota_bytes: number; max_active_jobs: number };
}

/// Server-reported capabilities - same shape and states as the desktop
/// `get_capabilities` command, so the existing gating logic applies as-is.
export async function getWebCapabilities(): Promise<Capability[]> {
  const response = await apiRequest<CapabilitiesResponse>("GET", "/capabilities");
  return response.capabilities;
}
