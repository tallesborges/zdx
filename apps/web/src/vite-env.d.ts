/// <reference types="svelte" />
/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly ZDX_DEV_INIT_DATA?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
