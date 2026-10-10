import {
  CLIENT_SURFACE_PROJECTION_READ_METHODS,
  type ClientSurfaceProjectionReadMethod,
} from "../generated/projectionReadContract";

export const CLIENT_SURFACE_READ_METHODS = CLIENT_SURFACE_PROJECTION_READ_METHODS;

export type ClientSurfaceReadMethod = ClientSurfaceProjectionReadMethod;

export const DEFAULT_CLIENT_SURFACE_COLLECTION_LIMIT = 50;
export const MAX_CLIENT_SURFACE_COLLECTION_LIMIT = 100;

const CLIENT_SURFACE_READ_METHOD_SET = new Set<string>(CLIENT_SURFACE_READ_METHODS);

export function isClientSurfaceReadMethod(value: string): value is ClientSurfaceReadMethod {
  return CLIENT_SURFACE_READ_METHOD_SET.has(value);
}

const EMBEDDED_IMAGE_PATTERN = /^data:image\/(?:avif|gif|jpeg|png|webp);base64,[a-z0-9+/]+={0,2}$/i;

export function isSafeClientSurfaceImageSource(value: string): boolean {
  return value.length <= 512 * 1024 && EMBEDDED_IMAGE_PATTERN.test(value);
}
