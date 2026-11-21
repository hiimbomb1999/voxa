# Hướng dẫn sử dụng Voxa

[← Về giới thiệu dự án](../README.md) · [Xem video demo ở đầu README](../README.md#see-voxa-in-action)

Phiên dịch âm thanh trực tiếp trên macOS bằng Rust và Tauri, với Gemini Live hoặc Soniox. Cần macOS 14.2 trở lên. Bản thử hiện được đóng gói cho Apple Silicon.

## Tải Voxa

Chọn phiên bản ở [GitHub Releases](https://github.com/hiimbomb1999/voxa/releases). Giải nén gói macOS để mở `Voxa.app`, hoặc gói Windows để mở `voxa.exe`. Xem [yêu cầu hệ thống](../README.md#download) trước khi tải.

## Windows 11 x64 bản thử

Xem cách tải hoặc tự build ở [mục Windows trong README](../README.md#windows-11-x64-preview). Bản thử dùng WASAPI để thu toàn hệ thống và hỗ trợ Gemini, Soniox. Để gửi tiếng Anh vào cuộc họp, chọn **CABLE Input** ở Voxa và **CABLE Output** làm micro trong Meet/Zoom. Mở **Nói trong cuộc họp → Thiết lập micro cho Meet / Zoom** để thử nhận giọng và nghe lại qua cáp. Kiểm thử âm thanh trên thiết bị Windows thực tế vẫn đang chờ.

## Mở và dịch video trên macOS

1. Sau khi build ở thư mục gốc của repo, mở `release/Voxa.app`. Trong **Cài đặt**, dán API key của Gemini hoặc Soniox. Voxa giữ key trong macOS Keychain.
2. Trong **Phiên dịch**, chọn **Toàn hệ thống** hoặc một ứng dụng đang phát tiếng, rồi chọn ngôn ngữ đích. Nếu tên ứng dụng chưa xuất hiện, phát tiếng và bấm **Làm mới thiết bị**.
3. Bấm **Bắt đầu dịch** và cấp quyền **System Audio Recording** cho Voxa khi macOS hỏi. Phụ đề có thể hiển thị nổi, song ngữ hoặc chỉ bản dịch.
4. **Trực tiếp** dùng âm thanh do Gemini Live Translate trả về. Các giọng có tên dùng TTS từ phụ đề đã nhận nên có thể xuất hiện muộn hơn. Khi dùng chế độ chỉ phụ đề, bỏ chọn **Phát giọng dịch**.

Mở **Tùy chọn phát** để xem độ trễ lần đầu, thời gian chờ trong hàng đợi và tổng lượng âm bị bỏ qua. Các số liệu hàng đợi cập nhật trong suốt phiên. Chiều nghe ưu tiên bám sát nguồn. Nếu nhiều đoạn đến dồn, Voxa có thể bỏ tiếng của đoạn đã quá trễ và giữ bản dịch trong phụ đề. Voxa xử lý âm thanh hệ thống nên hình ảnh vẫn do ứng dụng phát video điều khiển.

**Giảm tiếng gốc khi phát bản dịch** hạ tiếng nguồn còn khoảng 8% khi Voxa phát giọng dịch và tự tăng lại khi bản dịch ngừng. Chọn một ứng dụng làm nguồn nếu muốn hạ tiếng riêng ứng dụng đó. Chế độ này phát cả tiếng gốc và bản dịch qua thiết bị bạn chọn ở **Tùy chọn phát**. Dừng lượt nghe để bật hoặc tắt tùy chọn.

## Dịch trực tiếp với Soniox

Lấy API key trong [Soniox Console](https://console.soniox.com), rồi vào **Cài đặt → Soniox API** để lưu và kiểm tra model dịch. Chọn **Soniox · stt-rt-v6** ở đầu màn hình **Phiên dịch**. Lựa chọn này áp dụng cho chiều nghe và mic tiếng Việt sang tiếng Anh qua BlackHole. Dùng giọng Soniox Adrian hoặc chọn VieNeu-TTS nếu dùng tiếng Việt/Anh và đã cài bộ giọng. Tắt **Phát giọng dịch** nếu chỉ cần phụ đề. Tóm tắt và trợ lý dùng Gemini API key riêng.

## Giọng VieNeu-TTS trên Mac

Cài một lần bằng `./scripts/install-vieneu.sh` (cần [uv](https://docs.astral.sh/uv/getting-started/installation/)). Script tạo môi trường Python riêng, tải trọng số v3 Turbo và kiểm tra hai ngôn ngữ. Trong Voxa, vào **Cài đặt → VieNeu-TTS** để xem bộ giọng, chọn giọng cho chiều nghe hoặc cuộc họp và nghe thử qua loa hoặc tai nghe. Sau khi cài, bấm **Làm mới giọng**.

- Trong **Nghe bản dịch → Tùy chọn phát**, chọn tiếng Việt hoặc tiếng Anh, chọn giọng thuộc **VieNeu-TTS · Trên Mac**, rồi bấm **Nghe thử giọng**.
- Trong **Nói trong cuộc họp**, chọn giọng tiếng Anh và bấm **Nghe thử giọng** để nghe qua loa hoặc tai nghe đã chọn ở chiều nghe. **Gửi âm thử** phát câu tiếng Anh vào BlackHole. Hai nút này dùng được khi chưa nhập Gemini API key.
- Khi dịch trực tiếp, Gemini hoặc Soniox cung cấp văn bản bản dịch. VieNeu-TTS bắt đầu đọc khi nhận được một câu đủ trọn và tiếp tục đọc các đoạn kế tiếp. Chọn **Trực tiếp · Độ trễ thấp** để phát luồng âm thanh Gemini. **Tùy chọn phát** hiển thị thời gian từ đoạn âm gốc có tín hiệu đầu tiên được gửi đến phụ đề dịch và lúc bắt đầu phát giọng dịch.
- [VieNeu-TTS v3 Turbo](https://huggingface.co/pnnbao-ump/VieNeu-TTS-v3-Turbo#usage-rights-licensing-faq) phát hành trọng số và giọng cài sẵn theo Apache 2.0.

## Gửi tiếng Anh vào Meet hoặc Zoom

1. Cài **BlackHole 2ch** từ [Existential Audio](https://github.com/ExistentialAudio/BlackHole/wiki/Use-BlackHole-as-a-Virtual-Microphone). Có thể dùng `brew install --cask blackhole-2ch`, macOS sẽ yêu cầu xác nhận quản trị để cài.
2. Trong Meet/Zoom, chọn **BlackHole 2ch** làm microphone. Giữ **loa cuộc họp** ở tai nghe hoặc loa thật. Giữ ngõ ra mặc định của Mac ở loa/tai nghe thật.
3. Trong Voxa, chọn micro thật của bạn và **Gửi qua BlackHole 2ch**. Bấm **Gửi âm thử** khi chưa chạy dịch để thử riêng đường âm thanh.
4. **Zoom** có nút **Settings → Audio → Test Mic**. Bấm nút, gửi âm thử từ Voxa, rồi nghe Zoom phát lại qua tai nghe. Bạn cũng có thể bật dịch micro trước khi bấm **Test Mic** và nói một câu ngắn để nghe lại bản dịch thật.
5. **Google Meet** hiển thị vạch mức micro ở màn hình trước khi tham gia. Bấm **Gửi âm thử** để kiểm tra vạch mức. Để tự nghe tiếng như khách họp, vào phòng thử trên một điện thoại, tắt micro điện thoại và nghe từ điện thoại. Trên Mac, bỏ tắt tiếng micro Meet để truyền âm và dùng tai nghe để giảm vọng.
6. Bấm **Bật dịch micro**, cấp quyền micro và chờ **Giữ phím để nói**. Giữ **Control + Shift + Space** khi nói tiếng Việt, rồi thả phím. Bạn cũng có thể giữ nút ở vùng trạng thái bên trên phụ đề. Dải sáng phản ứng theo tín hiệu micro và đổi sang màu lam khi Voxa đang phát bản dịch. Đầu ra tiếng Anh đi vào BlackHole, chiều nghe đi vào loa hoặc tai nghe.
7. Để dùng lại micro thật với cuộc gọi, đổi thiết bị đầu vào ngay trong Meet/Zoom.

Trong **Thiết lập micro cho Meet / Zoom**, chọn **Nhạy** nếu nói nhỏ hoặc mất đầu câu, và **Lọc nền mạnh hơn** khi micro dễ nhận tiếng quạt. Voxa đợi bạn thả phím mới phát tiếng Anh vào BlackHole, rồi mở lượt nói mới sau khi âm thanh đã phát xong. Bạn có thể tạm tắt tiếng chiều nghe trong lượt nói và vẫn theo dõi phụ đề.

Bấm **Thử nhận giọng** để kiểm tra độ nhạy theo cùng bộ phát hiện của phiên thật. Bấm **Nghe lại BlackHole** hoặc **Nghe lại VB-CABLE** để gửi hai tiếng bíp qua cáp rồi phát lại trên tai nghe đã chọn. Thử trước khi vào họp. Voxa giữ mẫu thử trong bộ nhớ trong lúc kiểm tra.

**Độ trễ lượt nói** hiển thị số đo gần nhất của Gemini Trực tiếp, Soniox và VieNeu trong lần mở ứng dụng hiện tại. Để so sánh, thử cùng một câu với từng giọng. Với Soniox và VieNeu, app còn tách thời gian chờ bản dịch và thời gian tạo giọng đến lúc phát. Số đo kết thúc ở ngõ ra BlackHole trên Mac hoặc VB-CABLE trên Windows.

Nếu thấy mã **1008**, mở **Cài đặt → Kiểm tra kết nối Live** để xác minh Gemini chấp nhận cấu hình bằng key đang lưu. Phép thử chỉ mở kết nối và không gửi âm thanh. Nếu lỗi xuất hiện sau khi phiên đã chạy, Voxa thử kết nối lại một lần.

## Lịch sử và trợ lý

**Lưu transcript** ghi văn bản và mốc thời gian trên thiết bị. Voxa ghi nhớ lựa chọn này giữa các lần mở app. Trong Lịch sử, bấm **Xem phiên cũ hơn** để duyệt các phiên trước hoặc **Xóa** để xóa từng phiên. Có thể xuất TXT, Markdown hoặc SRT. Mốc SRT tính từ lúc phiên Voxa bắt đầu và dựa trên thời điểm nhận bản dịch. Khi ghép vào video nguồn, mốc có thể cần căn lại. **Tóm tắt cuộc họp** gửi transcript đã chọn tới Gemini. Trợ lý giọng nói nằm trong **Cài đặt** và dùng micro riêng theo thao tác giữ-nói. Dừng dịch micro của cuộc họp trước khi bật trợ lý.

## Build và kiểm thử

Cần Rust stable và Xcode Command Line Tools.

```sh
cargo test --locked --features custom-protocol --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --all-targets --features custom-protocol --manifest-path src-tauri/Cargo.toml -- -D warnings
./scripts/bundle.sh
```

Gói `.app` được ký ad hoc để thử cục bộ. Kiểm thử tự động dùng dữ liệu tổng hợp. Để xác minh độ trễ và chiều nói thực tế, dùng key của bạn, BlackHole và một cuộc gọi thử có người nghe ở đầu kia. Đo riêng thời gian đến phụ đề, thời gian đến âm thanh và khả năng phục hồi sau khi mất mạng.

### Thử phiên 15 phút và âm thanh trong cuộc gọi

- Phát cùng một video 15 phút có lời nói và vài khoảng nghỉ rõ. Ghi độ lệch giữa một câu gốc và tiếng dịch ở đầu, giữa và cuối video. So sánh từng giọng với cùng đoạn video, ngôn ngữ và thiết bị phát.
- Trong **Tùy chọn phát**, ghi số đo lần đầu, đỉnh hàng đợi và tổng lượng âm gốc hoặc âm dịch bị bỏ qua. Khi thử mất mạng ngắn, kiểm tra app có quay lại phần đang phát hay còn đọc bản dịch của đoạn cũ.
- Trong một cuộc gọi thử Meet hoặc Zoom, bật cả hai chiều. Thử nói nhỏ và nói bình thường, kiểm tra **Đã nhận giọng** rồi **Đã phát bản dịch ra cáp cuộc họp**. Xác nhận âm ở thiết bị của người nghe, vì chỉ báo trong Voxa đo tại ngõ ra cáp.
- Đổi tai nghe và thử mất mạng giữa cuộc gọi. Ghi hệ điều hành, nhà cung cấp, thiết bị trước và sau khi đổi cùng thông báo của Voxa. Dùng nội dung thử có thể chia sẻ khi đính kèm báo lỗi.
