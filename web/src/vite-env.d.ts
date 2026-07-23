/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_ENABLE_SYNTHETIC_CAPTURE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
