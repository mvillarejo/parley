import { describe, expect, it } from "vitest";
import {
  canTranscribe,
  defaultProfileSettings,
  getActiveProfile,
  hasDraftErrors,
  uniqueProfileName,
  validateProfileDraft,
  validateSttBaseUrl,
  type NewProfile,
  type Profile,
  type ProfilesState,
} from "./sttProvider";

const gladia = (id: string, name: string, apiKey: string): Profile => ({
  id,
  name,
  kind: "gladia",
  api_key: apiKey,
  code_switching: false,
});
const local = (id: string, name: string): Profile => ({
  id,
  name,
  ...defaultProfileSettings("openai_compat"),
});
const whisper = (id: string, name: string, modelPath = ""): Profile => ({
  id,
  name,
  kind: "whisper_local",
  model_path: modelPath,
});
const state = (active: string, ...profiles: Profile[]): ProfilesState => ({
  profiles,
  active_profile_id: active,
});

describe("canTranscribe", () => {
  it("requires a Gladia key only for Gladia", () => {
    expect(canTranscribe(gladia("g", "G", ""))).toBe(false);
    expect(canTranscribe(gladia("g", "G", "   "))).toBe(false);
    expect(canTranscribe(gladia("g", "G", "key"))).toBe(true);
    expect(canTranscribe(local("l", "L"))).toBe(true);
    expect(canTranscribe(whisper("w", "W"))).toBe(true);
    expect(canTranscribe(null)).toBe(false);
  });

  it("follows the active profile, not any other profile", () => {
    const keyed = gladia("g", "Work", "key");
    const keyless = gladia("h", "Personal", "");
    const ollama = local("l", "Ollama");
    expect(canTranscribe(getActiveProfile(state("h", keyed, keyless)))).toBe(
      false,
    );
    expect(canTranscribe(getActiveProfile(state("g", keyed, keyless)))).toBe(
      true,
    );
    expect(canTranscribe(getActiveProfile(state("l", keyless, ollama)))).toBe(
      true,
    );
    expect(canTranscribe(getActiveProfile(state("gone", keyed)))).toBe(false);
  });
});

describe("validateProfileDraft", () => {
  const profiles = [gladia("g", "Gladia", "k"), local("l", "Local Ollama")];
  const draft = (patch: Partial<NewProfile> = {}): NewProfile =>
    ({
      name: "Groq",
      ...defaultProfileSettings("openai_compat"),
      ...patch,
    }) as NewProfile;

  it("accepts a valid new profile", () => {
    expect(validateProfileDraft(draft(), profiles, null)).toEqual({});
  });

  it.each(["", "   ", "gladia", " LOCAL ollama ", "x".repeat(61)])(
    "rejects the name %j",
    (name) => {
      expect(
        validateProfileDraft(draft({ name }), profiles, null).name,
      ).toBeDefined();
    },
  );

  it("lets a profile keep its own name when editing", () => {
    expect(
      validateProfileDraft(draft({ name: "local ollama" }), profiles, "l"),
    ).toEqual({});
  });

  it("checks base URL and model only for OpenAI-compatible profiles", () => {
    const errors = validateProfileDraft(
      draft({ base_url: "localhost:11434", model: " " } as Partial<NewProfile>),
      profiles,
      null,
    );
    expect(errors.base_url).toBeDefined();
    expect(errors.model).toBeDefined();
    expect(hasDraftErrors(errors)).toBe(true);
    const gladiaDraft: NewProfile = {
      name: "Gladia 2",
      ...defaultProfileSettings("gladia"),
    };
    expect(validateProfileDraft(gladiaDraft, profiles, null)).toEqual({});
  });
});

describe("uniqueProfileName", () => {
  it("numbers clashing names case-insensitively", () => {
    expect(uniqueProfileName("Groq", [])).toBe("Groq");
    expect(
      uniqueProfileName("Gladia", [
        gladia("a", "gladia", ""),
        gladia("b", "Gladia 2", ""),
      ]),
    ).toBe("Gladia 3");
  });
});

describe("validateSttBaseUrl", () => {
  it.each([
    "http://localhost:11434/v1",
    " https://api.groq.com/openai/v1/ ",
    "http://192.168.1.20:8000/v1",
  ])("accepts %s", (url) => {
    expect(validateSttBaseUrl(url)).toBeNull();
  });

  it.each(["", "   ", "localhost:11434/v1", "not a url", "ftp://host/v1"])(
    "rejects %j",
    (url) => {
      expect(validateSttBaseUrl(url)).not.toBeNull();
    },
  );
});

describe("defaultProfileSettings", () => {
  it("starts a Local Whisper profile with an empty model path", () => {
    expect(defaultProfileSettings("whisper_local")).toEqual({
      kind: "whisper_local",
      model_path: "",
    });
  });
});

describe("validateProfileDraft for Local Whisper", () => {
  const modelPath =
    "/Users/me/Library/Application Support/whisper.cpp/models/ggml-large-v3-turbo.bin";
  const draft = (path: string): NewProfile => ({
    name: "Local Whisper",
    kind: "whisper_local",
    model_path: path,
  });

  it("accepts an absolute model path", () => {
    expect(validateProfileDraft(draft(modelPath), [], null)).toEqual({});
    expect(validateProfileDraft(draft(` ${modelPath} `), [], null)).toEqual({});
  });

  it.each(["", "   "])("requires the model file (%j)", (path) => {
    expect(validateProfileDraft(draft(path), [], null).model_path).toBe(
      "Model file is required",
    );
  });

  it.each([
    "ggml-large-v3-turbo.bin",
    "~/Library/Application Support/whisper.cpp/models/ggml-large-v3-turbo.bin",
    "./models/ggml-base.bin",
  ])("rejects the relative path %j", (path) => {
    expect(validateProfileDraft(draft(path), [], null).model_path).toBe(
      "Use the full path to the model file",
    );
  });

  it("does not check URL or model fields", () => {
    const errors = validateProfileDraft(draft(""), [], null);
    expect(errors.base_url).toBeUndefined();
    expect(errors.model).toBeUndefined();
  });
});
