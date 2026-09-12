import { describe, expect, it } from "vitest";
import { derivedWidth, proxyPath } from "./lib";

describe("proxy dimensions", () => {
  it("produces the established Canon C50 1.5x raster", () => {
    expect(derivedWidth(6960, 4640, 1.5, 1080)).toBe(2430);
  });

  it("always returns an encoder-safe even width", () => {
    expect(derivedWidth(4096, 2160, 1.33, 720) % 2).toBe(0);
  });
});

describe("default output folder", () => {
  it("uses a Proxy directory under the source", () => {
    expect(proxyPath("/Volumes/Footage/Day 01")).toBe("/Volumes/Footage/Day 01/Proxy");
  });
});

