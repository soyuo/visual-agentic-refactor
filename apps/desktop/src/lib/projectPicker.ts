import { open } from "@tauri-apps/plugin-dialog";

import { AppCommandError } from "./commands";

export async function selectProjectFolder(): Promise<string | null> {
  try {
    const path = await open({
      title: "프로젝트 폴더 선택",
      directory: true,
      multiple: false,
    });

    return typeof path === "string" ? path : null;
  } catch {
    throw new AppCommandError(
      "internal",
      "프로젝트 폴더 선택 창을 열지 못했습니다.",
    );
  }
}
