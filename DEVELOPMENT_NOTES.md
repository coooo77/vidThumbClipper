# 開發筆記：踩過的坑與原因

這份筆記記錄開發過程中發現的、比較不直覺、容易在之後被誤改回去的問題。每一條都包含：症狀、原因、修法、相關檔案。

---

## 1. ffmpeg `-ss` / `-to` 的位置，效果差很多

**症狀**：剪輯長時間錄影（36+ 小時）中，時間點越深入檔案的片段，越容易輸出空檔案（幾百 KB，播放器打不開），即使 `-ss`/`-to` 數值本身是對的。

**原因**：`-ss` 放在 `-i` **前面**是輸入端快速 seek，靠檔案內部的索引/PCR 資訊直接跳過去；放在 `-i` **後面**是輸出端精確 seek，靠從檔案真正開頭依序掃描每個封包的時間戳來定位，完全不依賴索引。這兩種方式看起來只是「參數順序不同」，實際行為差很多：

- 快速 seek：速度快，但如果來源檔案的索引/時間戳有問題（見第 2 點），跳過去的位置可能整個錯誤，甚至跳到「檔案認知範圍之外」，導致輸出完全空白。
- 精確 seek：一定正確，但深入大檔案的位置需要先掃過前面所有內容，可能要等好幾分鐘。

**修法**：`Tasks.vue` 的 `clipRange()` 現在依照每個任務的 `preciseSeek` 欄位（在「建立處理任務」卡片可以勾選「精確 seek」）決定用哪一種：
- 不勾（預設）：`-ss` 放 `-i` 前面，快速但可能對問題檔案失準。
- 勾選：`-i` 放最前面，`-ss`/`-to` 都放後面，慢但保證正確。

`-to`（絕對結束時間）本身不管放哪一種模式，都固定放在 `-i` 後面——它跟 `-t`（相對長度）不一樣，`-t` 是從實際 seek 落點開始算，seek 不準時結束點也會跟著偏移；`-to` 則是對照原始檔案的絕對時間，落點不準時只有起點稍微提前，結束點永遠準。

**相關檔案**：`Tasks.vue`（`clipRange`）、`Home.vue`（`preciseSeek` checkbox）、`taskStore.ts`（`VideoTask.preciseSeek`）

---

## 2. 超長時間錄影的 PTS/PCR 回捲與索引損毀

**症狀**：`ffmpeg -i` 探測到的 `Duration` 遠短於實際檔案內容（例如實際 36 小時，探測出來只有 10 小時多）；用快速 seek 去到「探測長度之外」的時間點，直接得到空輸出。

**原因**：MPEG-TS 的 PTS/PCR 時間戳是有限位元數的計數器，長時間連續錄影（尤其中途斷線重連過的直播錄影）容易讓時間戳不連續或回捲，導致 ffmpeg 對檔案長度的探測整個算錯，連帶讓依賴索引的快速 seek 失準。

**重要教訓**：這個問題**不能**靠「先 remux 成 mp4 再剪」完全繞過。Remux（`-c copy`）在處理這種問題檔案時，可能會靜靜丟掉/錯位一部分內容（實測 remux 前後片長差了 6.5 分鐘），導致剪出來的內容跟預期對不上、甚至時長異常拉長。**對這類問題檔案，剪輯要用原始檔案 + 精確 seek（見第 1 點），不要用 remux 過的版本。**

Remux／預先處理功能仍然有價值：它能修正縮圖擷取時的時間軸誤差（grid 預覽用），只是不該被當作「剪輯來源的安全替代品」。

**相關檔案**：`Home.vue`（預先處理功能）、`Tasks.vue`（`runPreprocessTask`）

---

## 3. ffmpeg exit code 0 不代表真的有輸出內容

**症狀**：`capture_single_thumbnail`、`clipRange` 有時回報成功（exit code 0），但實際檔案不存在，或存在卻只有幾百 KB、無法播放。

**原因**：ffmpeg 在某些邊界情況（例如色彩空間不支援、seek 落點超出範圍）會回報成功結束，但實際上沒有寫入任何可用的影音資料。更麻煩的是，`get_video_duration`（讀 MP4 container 的 `format=duration`）**不可靠**——壞掉的 MP4 header 可能仍然宣告跟要求一致的長度，就算實際內容是空的。

**修法**：
- `capture_single_thumbnail`：exit code 成功之外，額外檢查 `image_path.exists()`，兩個條件都要過才算真的成功；否則把 ffmpeg 完整 stdout/stderr 一起回傳。
- `clipRange`：不看 `get_video_duration`，改用新增的 `get_file_size` command 直接檢查**實際檔案大小**（< 200KB 視為異常），並把完整的實際指令跟 ffmpeg 輸出一起塞進錯誤訊息，方便下次診斷。

**相關檔案**：`lib.rs`（`capture_single_thumbnail`、`get_file_size`）、`Tasks.vue`（`clipRange`）

---

## 4. MJPEG 編碼器 + 非 full-range YUV 來源

**症狀**：`capture_single_thumbnail` 對某些廣播/直播錄影來源失敗，錯誤訊息是 `Non full-range YUV is non-standard, set strict_std_compliance to at most unofficial to use it`，接著 `ff_frame_thread_encoder_init failed`。

**原因**：來源是 `yuv420p(tv, ...)`（limited/tv range），MJPEG 編碼器對這種非標準色彩範圍預設會拒絕。

**修法**：擷取縮圖時加上 `-pix_fmt yuvj420p`（明確轉成 full-range，這是 JPEG 慣例的格式）跟 `-strict unofficial`（放寬相容性檢查作為保險）。

**副作用（已知取捨，未解決）**：這個轉換對每一張縮圖都要做一次色彩空間換算，會讓截圖速度變慢。目前沒有做「先偵測來源是不是 tv-range，只在需要時才轉換」的最佳化——如果之後這個速度問題變得無法接受，可以往這個方向優化。

**相關檔案**：`lib.rs`（`capture_single_thumbnail`）

---

## 5. Windows 上子行程會閃出主控台視窗

**症狀**：打包後在其他電腦執行，呼叫 ffmpeg/ffprobe 時會閃一下 cmd 黑窗。

**原因**：這個 app 是 GUI 程式，沒有自己的主控台；Windows 對它產生的主控台子行程（ffmpeg.exe 是主控台程式）預設會另外配置一個新視窗。

**修法**：`lib.rs` 新增 `spawn_hidden()`，Windows 專屬用 `CREATE_NO_WINDOW`（`0x08000000`）flag 包住所有 `Command::new`，其他平台不受影響（`#[cfg(target_os = "windows")]`）。

**相關檔案**：`lib.rs`（`spawn_hidden`，所有 ffmpeg/ffprobe 呼叫都要走這個函式，不要直接用 `Command::new`）

---

## 6. Tauri capability 權限系統，跟 `tauri.conf.json` 是分開的兩件事

**症狀**：`openPath()` 開啟資料夾失敗，錯誤是 `Not allowed to open path ...`，但 `tauri.conf.json` 看起來完全正常。

**原因**：Tauri v2 的視窗/外掛權限是另外用 `src-tauri/capabilities/*.json` 管理，不在 `tauri.conf.json` 裡。而且權限用**純字串**列出（例如 `"opener:allow-open-path"`）時，隱含的允許範圍（scope）是空的——等於權限本身有了，但沒有任何路徑被允許，還是會被擋。

**修法**：權限要用**帶 `allow` 清單的物件形式**明確列出允許的路徑範圍：
```json
{
  "identifier": "opener:allow-open-path",
  "allow": [{ "path": "$TEMP/**" }]
}
```
之後如果加新的視窗/外掛操作（例如置頂功能用到 `core:window:allow-set-always-on-top`），一律先假設需要額外加權限，遇到「功能明明寫對了卻沒反應」要先往這個方向查，不要只看 `tauri.conf.json`。

**相關檔案**：`src-tauri/capabilities/default.json`

---

## 7. WebView2 裡原生 `title` 屬性 tooltip 不可靠

**症狀**：用 `<span title="...">` 做 hover 說明，滑鼠移上去只看到游標變成 `?`（`cursor: help` 的效果），但說明文字的原生 tooltip 沒有跳出來。

**原因**：Tauri 用的 WebView2，對原生 `title` tooltip 的支援不穩定，不能假設它一定會顯示。

**修法**：不要依賴瀏覽器原生 tooltip；改用自己刻的點擊展開式說明框（一個 ref 控制顯示/隱藏 + 一個 `document` 層級的 click 監聽做「點外面關閉」），完全不靠原生行為。

**相關檔案**：`Home.vue`（`showPreprocessHint`、`.info-popover`）

---

## 8. `App.vue` 全域樣式跟元件內 class 名稱意外撞名

**症狀**：`Tasks.vue` 的「清除已完成」「開始依序執行」按鈕，跟下面「選擇資料夾」那一行畫面重疊、跑版，但檢查 DOM 結構完全正常（是不同行）。

**原因**：`App.vue` 的 `<style>` 沒有加 `scoped`（因為要放 `.btn`、`.card` 這些全站共用樣式），裡面新增的 `.header-actions`（給頂部標題列用，`position: absolute` 定位）是全域生效的。`Tasks.vue` 自己也用了同一個 class 名字 `.header-actions`（按鈕群組），`<style scoped>` 沒有覆寫 `position` 相關屬性，全域規則就滲透進來，把按鈕拉出正常文件流、變成絕對定位飄在畫面上。

**修法**：`Tasks.vue` 的 class 改名成 `.task-actions`，徹底避開撞名。

**教訓**：`App.vue` 之後如果要新增全域 class，最好取比較不容易被其他元件重複使用的名字（或至少在新增時，順手搜尋一次專案裡有沒有其他檔案用了同名 class）。已經全部掃過一次確認沒有其他撞名風險（`.input-group`/`.input-with-btn` 是刻意共用的設計系統基礎樣式，不算意外撞名）。

**相關檔案**：`App.vue`、`Tasks.vue`

---

## 9. 縮圖選取狀態要用「穩定值」而不是「陣列 index」

**症狀**：使用者選好兩張縮圖準備做下一步操作時，如果背景有另一個截圖佇列任務同時完成、插入了新縮圖，使用者原本選取的位置會「跑掉」，變成選到別的縮圖。

**原因**：選取狀態原本存的是縮圖在陣列裡的 index。截圖佇列是背景持續執行的，一旦有新縮圖被插入陣列中間，插入點之後所有縮圖的 index 都會往後挪，但使用者手上存的 index 沒有跟著更新，就會指向錯誤的縮圖。

**修法**：選取狀態改成存縮圖的**時間值**（`selectedTimes`，不是 index），時間值不會因為陣列被插入新元素而改變。真正需要知道「現在在陣列的第幾個位置」時（例如判斷兩張是否相鄰），才即時用 `indexForTime()` 查詢當下位置，不依賴任何提早存好、可能過期的 index。

**教訓**：任何「使用者選取的東西」如果它所在的陣列會被非同步、背景的操作修改，選取狀態就不能存 index，要存能唯一識別該項目的穩定值（ID、時間戳等）。

**相關檔案**：`Home.vue`（`selectedTimes`、`toggleSelection`、`selectedPair`、`indexForTime`）

---

## 10. 任務佇列處理迴圈要用「即時查詢」，不要「一次性快照」

**症狀**：任務／截圖佇列正在執行時，使用者中途加入新任務，等目前任務跑完後，新加入的任務不會被接續執行。

**原因**：迴圈寫成 `for (const task of videoTasks.filter(...))`，`.filter()` 在迴圈開始那一刻就把陣列拍成快照，之後加入的項目不在那份快照裡。

**修法**：改成 `for (;;) { const task = videoTasks.find(...); if (!task) break; ... }`，每次迭代都重新查詢當下狀態為「等待中」的項目，只要佇列裡還有東西就會繼續處理，新加入的自然會被撿到。

**相關檔案**：`Tasks.vue`（`runTasks`）、`Home.vue`（`processExpansionQueue`）

---

## 11. `AudioContext` 可能以 `suspended` 狀態建立

**症狀**：程式產生的提示音效有時候完全沒聲音，沒有任何錯誤。

**原因**：瀏覽器（WebView2 也一樣）的音訊政策，`AudioContext` 建立後可能預設是 `suspended`，需要明確呼叫 `resume()` 才會真的發聲——尤其這裡是任務**非同步跑完之後**才觸發播放（不是使用者點擊當下同步執行），更容易遇到。

**修法**：建立 `AudioContext` 後檢查 `ctx.state === "suspended"`，是的話先 `resume()`、等它真的恢復了才排程播放，不是就直接播。

**相關檔案**：`notify.ts`（`playTone`）

---

## 12. 初始縮圖的 N × M 格數要包含起點與終點

**症狀**：選擇 4 × 5 建立初始縮圖時，畫面出現 20 張中間縮圖，再加上起點與終點，總共 22 張，超出使用者選定的格數。

**原因**：原本把 `gridCols * gridRows` 全部拿來產生中間縮圖，之後又額外渲染起點與終點。端點其實也是版面中的兩個格子，不能重複加在 N × M 之外。

**修法**：初始中間縮圖數量改成：

```ts
Math.max(0, gridCols * gridRows - 2)
```

因此 4 × 5 會產生 18 張中間縮圖，加上起點與終點後正好 20 格。UI 說明文字也要明確標示 N × M 是包含端點的總格數。

**相關檔案**：`Home.vue`（`initialGridSlotCount`、`initialThumbnailCount`、`generateGridThumbnails`）

---

## 13. 起點／終點縮圖不能阻塞初始縮圖與擴充佇列

**症狀**：中間縮圖已完成，但起點／終點沒有畫面；進度文字仍停在「正在擷取起點與終點縮圖…」，擴充按鈕 disabled，或使用者加入的擴充任務一直停在「等待中」。

**原因**：初始流程用 `Promise.all` 等待起點與終點。某些長時間 MP4 在接近容器結尾的位置會讓 ffmpeg 回報 exit code 0 卻輸出空檔，接著 precise seek 又可能長時間掃描；整個 `isProcessing` 因此無法解除。另一方面，起點／終點只是 UI 端點預覽，不應該阻塞已經完成的中間縮圖或擴充任務。

**修法**：

- 中間縮圖完成後立即解除 `isProcessing`，起點／終點改在背景擷取。
- 起點與終點使用 `Promise.allSettled` 分開處理；其中一張失敗不能讓另一張也消失。
- 終點不要直接使用 `duration - 0.5`，改用較安全的 `duration - 2`，避免落在最後一個不可解碼的時間戳。
- 端點擷取失敗只寫入 Home logger，不阻塞擴充功能。
- 初始流程成功或失敗後都清除進度文字，避免殘留看似仍在處理的狀態。

**相關檔案**：`Home.vue`（`isEndpointProcessing`、`captureEndpointThumbnails`、`generateGridThumbnails`）、`lib.rs`（`capture_single_thumbnail`）

---

## 14. 擴充任務執行中仍要允許加入後續任務

**症狀**：第一個擴充任務執行到一半時，使用者無法再加入其他截圖任務；或任務被加入後停在「等待中」且沒有繼續執行。

**原因**：擴充任務本身是序列佇列，`isExpanding` 代表 worker 正在工作，不代表不能新增等待中的任務。如果把 `isExpanding` 加入「加入截圖任務」按鈕的 disabled 條件，便違反原本的設計。若在 `isProcessing` 期間加入任務，`processExpansionQueue()` 會提早返回，也需要在初始縮圖真正完成後重新啟動佇列。

**修法**：

- 擴充按鈕只在初始縮圖 `isProcessing` 或同一範圍 `isPairLocked` 時停用；`isExpanding` 期間仍可加入其他範圍。
- `processExpansionQueue()` 使用即時 `find()` 取得下一個「等待中」任務；目前任務完成後會自動接續中途新增的任務。
- 若舊狀態已經留下等待中的任務，初始縮圖完成後重新呼叫 `processExpansionQueue()`。
- 沒有可插入時間點時不能靜默標記為完成，必須在 Home logger 顯示原因。

**相關檔案**：`Home.vue`（`queueExpansionJob`、`runExpansionJob`、`processExpansionQueue`）

---

## 15. ffmpeg 截圖 exit code 0 不代表真的產生圖片

**症狀**：ffmpeg 顯示 exit code 0，但輸出檔不存在或是空檔；例如長時間影片接近結尾時出現：`Output file is empty, nothing was encoded`。如果只檢查 exit code，前端會誤以為截圖成功。

**原因**：輸入端 `-ss` 的快速 seek 依賴容器索引與時間戳。在某些 MP4 或異常時間軸中，ffmpeg 可以正常結束，但實際找不到可解碼的畫面。改用輸出端 precise seek 雖然較可靠，卻可能從檔頭掃描很久。

**修法**：`capture_single_thumbnail` 必須同時檢查：

- process exit code 成功；
- 輸出路徑是檔案；
- 檔案大小大於 0。

fast seek 失敗時可嘗試 precise seek，但兩種流程都必須有 timeout，並在 timeout 後 kill、wait、join stdout/stderr reader，避免背景程序或 reader thread 殘留。錯誤訊息要包含 fast/precise 兩次嘗試的耗時、exit code、檔案狀態、實際指令與 ffmpeg stderr，讓 Home logger 能直接提供診斷資料。

**相關檔案**：`lib.rs`（`run_with_timeout`、`capture_single_thumbnail`、`thumbnail_attempt_debug`）

---

## 16. 精確 seek 與快速 seek 的使用取捨

**差異**：

- 快速 seek：`-ss` 放在 `-i` 前，速度快，但可能從前一個 keyframe 開始，剪輯起點會有偏移。
- 精確 seek：`-ss` 放在 `-i` 後，會更接近要求的時間，但長影片需要從前面掃描，速度可能慢很多。

目前剪輯使用 `-c copy`，因此精確 seek 也不等同於逐影格完全精準；若需要真正逐影格精準，必須重新編碼，代價是速度變慢、檔案可能變大或產生畫質差異。

**建議**：一般正常 MP4 使用快速 seek；長時間錄影、TS/remux 後時間軸異常或剪輯起點明顯偏移時，才啟用精確 seek。

**相關檔案**：`Tasks.vue`（`clipRange`、`preciseSeek`）

---

## 17. 截圖取消與單張總期限

截圖現在由前端先註冊獨立 request ID，再呼叫擷取，最後釋放註冊。取消只通知該 ID 的後端旗標；執行迴圈每 150ms 檢查並終止目前程序。註冊期間取消會阻止擷取開始，兩次嘗試之間取消也不會繼續重試。背景起終點截圖使用獨立 ID，不受初始／擴充取消影響。

每張截圖從後端收到擷取要求起共用 30 秒期限，包含阻塞工作排隊時間。快速 seek 最多 20 秒，precise seek 只使用剩餘時間，不再額外給 120 秒。程序退出、回收及輸出讀取的清理階段最多再等 2 秒；若清理未完成，回報異常，由背景清理繼續接手，且不啟動 fallback。此期限不代表同步檔案系統操作也具備硬性中斷能力。

初始與擴充縮圖改為逐張插入畫面；後續失敗或取消不會丟失同批已完成圖片。擴充任務取消時顯示「正在取消…」並停用重複取消。

驗證：Rust 子程序測試涵蓋執行中取消、兩次嘗試共用期限、大量 stdout/stderr 不阻塞；前端另驗證註冊期間取消、執行中取消及逾時保留已完成圖片。

---

## 18. 任務再編輯只保存時間軸，不保存縮圖圖片

剪輯與剪輯＋合併任務建立時，把當下可見縮圖的時間點（四捨五入到毫秒）、影片長度與欄數保存到 `previewTimeline`。不保存 JPEG/base64 圖片；再編輯時才用來源影片依序重新擷取，因此任務刪除不需要額外清理圖片檔。以一萬個一般時間點估算，JSON 約為數十到一百多 KB，遠小於保存一萬張 JPEG。

等待中任務再編輯後原地更新；完成、失敗、已取消任務保留原紀錄，另建新的等待任務；處理中的任務不允許再編輯。舊版任務沒有 `previewTimeline` 時仍可載入欄位，但必須重新建立縮圖。

任務的 `segments` 也保存建立當下計算完成的實際範圍，因此「依時間長度」與「依數量分割」都能在任務頁展開查看精確片段，執行時直接使用同一份範圍，避免畫面顯示與實際輸出不一致。

**相關檔案**：`taskStore.ts`（`TaskPreviewTimeline`、再編輯請求）、`Home.vue`（保存與恢復時間軸）、`Tasks.vue`（範圍明細與再編輯入口）
