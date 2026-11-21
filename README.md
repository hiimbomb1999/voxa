# Voxa

Phiên dịch âm thanh trực tiếp trên macOS bằng Rust, Tauri và Gemini Live API. Cần macOS 14.2 trở lên. Bản thử hiện được đóng gói cho Apple Silicon.

## Mở và dịch video

1. Mở `release/Voxa.app`. Trong **Cài đặt**, dán Gemini API key. Key nằm trong macOS Keychain.
2. Trong **Phiên dịch**, chọn **Toàn hệ thống** hoặc một ứng dụng đang phát tiếng, rồi chọn ngôn ngữ đích. Nếu tên ứng dụng chưa xuất hiện, phát tiếng và bấm **Làm mới thiết bị**.
3. Bấm **Bắt đầu dịch** và cấp quyền **System Audio Recording** cho Voxa khi macOS hỏi. Phụ đề có thể hiển thị nổi, song ngữ hoặc chỉ bản dịch.
4. **Trực tiếp** dùng âm thanh do Gemini Live Translate trả về. Các giọng có tên dùng TTS từ phụ đề đã nhận nên có thể xuất hiện muộn hơn. Khi dùng chế độ chỉ phụ đề, bỏ chọn **Phát giọng dịch**.

## Gửi tiếng Anh vào Meet hoặc Zoom

1. Cài **BlackHole 2ch** từ [Existential Audio](https://github.com/ExistentialAudio/BlackHole/wiki/Use-BlackHole-as-a-Virtual-Microphone). Có thể dùng `brew install --cask blackhole-2ch`, macOS sẽ yêu cầu xác nhận quản trị để cài.
2. Trong Meet/Zoom, chọn **BlackHole 2ch** làm microphone. Giữ **loa cuộc họp** ở tai nghe hoặc loa thật. Giữ ngõ ra mặc định của Mac ở loa/tai nghe thật.
3. Trong Voxa, chọn micro thật của bạn và **Gửi qua BlackHole 2ch**. **Phát âm thử** sẽ gửi một tiếng ngắn vào BlackHole để xem mức micro trong phần kiểm tra thiết bị của Meet/Zoom.
4. Bấm **Bật dịch micro**, cấp quyền micro và chờ **Giữ phím để nói**. Giữ **Control + Shift + Space** khi nói tiếng Việt, rồi thả phím. Bạn cũng có thể giữ nút trên giao diện. Đầu ra tiếng Anh đi vào BlackHole, chiều nghe đi vào loa hoặc tai nghe.
5. Để dùng lại micro thật với cuộc gọi, đổi thiết bị đầu vào ngay trong Meet/Zoom.

## Lịch sử và trợ lý

**Lưu transcript** lưu văn bản và mốc thời gian trên Mac, không lưu bản ghi âm. Có thể xuất TXT, Markdown hoặc SRT. SRT xuất các đoạn bản dịch hoặc phản hồi. **Tóm tắt cuộc họp** gửi transcript đã chọn tới Gemini. Trợ lý giọng nói nằm trong **Cài đặt** và dùng micro riêng theo thao tác giữ-nói. Dừng dịch micro của cuộc họp trước khi bật trợ lý.

## Build và kiểm thử

Cần Rust stable và Xcode Command Line Tools.

```sh
cargo test --locked --features custom-protocol --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --all-targets --features custom-protocol --manifest-path src-tauri/Cargo.toml -- -D warnings
./scripts/bundle.sh
```

Gói `.app` được ký ad hoc để thử cục bộ. Kiểm thử tự động dùng dữ liệu tổng hợp. Để xác minh độ trễ và chiều nói thực tế, dùng key của bạn, BlackHole và một cuộc gọi thử có người nghe ở đầu kia. Đo riêng thời gian đến phụ đề, thời gian đến âm thanh và khả năng phục hồi sau khi mất mạng.
