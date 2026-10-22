# 모델 팩

ViewPort는 이미지 분석 모델을 포함하지 않습니다. 모델마다 라이선스가 다르므로 사용자가 직접 받아 설치합니다.
모델 팩 하나는 `manifest.json`과 그 파일이 가리키는 ONNX 파일(태거는 라벨 CSV까지)이 든 폴더입니다.

앱은 다음 위치에서 모델 팩 폴더를 찾습니다.

1. 앱 실행 파일 옆의 `models/`
2. 설정 폴더의 `models/` (Windows: `%APPDATA%\<앱 id>\models`)
3. 설정 → 분석 → "모델 팩 폴더"에 추가한 폴더

각 위치는 그 자체가 팩 폴더이거나, 바로 아래 하위 폴더들이 팩 폴더일 수 있습니다.

## ONNX Runtime

모델은 ONNX Runtime으로 실행합니다. ONNX Runtime 역시 포함하지 않으며, 앱이 실행 중에 라이브러리를 불러옵니다(1.17 이상).

| GPU | 받을 것 | 실행 장치 |
|---|---|---|
| NVIDIA | `onnxruntime-gpu` 빌드의 `onnxruntime.dll`과 `onnxruntime_providers_*.dll`, 그리고 CUDA 12·cuDNN 9 런타임 | `cuda` |
| 그 밖의 Windows GPU(AMD, Intel, NVIDIA) | `onnxruntime-directml` 빌드의 `onnxruntime.dll`(+ `DirectML.dll`) | `directml` |
| 없음 | 일반 빌드 | `cpu` |

CUDA 빌드와 DirectML 빌드는 서로 다른 라이브러리입니다. 설정 → 분석의 "ONNX Runtime 위치"에 쓰려는 빌드의 파일이나 폴더를
지정하세요. "실행 장치 순서"(기본 `cuda`, `directml`, `cpu`)를 앞에서부터 시도해 처음 되는 장치를 씁니다. Linux와 macOS
빌드에는 DirectML이 없습니다. 휴대폰 빌드는 분석을 실행하지 않고, PC에서 분석한 결과만 보여 줍니다.

## manifest.json

```json
{
  "id": "yunet",                         // 설정이 가리키는 id
  "name": "YuNet",
  "version": "2023mar",                  // 가중치가 바뀌면 올린다: 다른 모델의 특징값은 섞이지 않는다
  "kind": "face_detector",               // face_detector | face_embedder | anime_detector | anime_embedder | tagger
  "format": "yunet",                     // yunet | yolo | embedding | wd_tagger
  "model": "face_detection_yunet_2023mar.onnx",
  "labels": null,                        // 태거: selected_tags.csv
  "input": {
    "width": 640, "height": 640,
    "layout": "nchw",                    // nchw | nhwc
    "channels": "bgr",                   // rgb | bgr
    "scale": 1.0,                        // 값 = (픽셀(0~255) × scale − mean) / std
    "mean": [0, 0, 0], "std": [1, 1, 1],
    "fit": "letterbox",                  // letterbox(오른쪽·아래 여백) | pad_square(가운데 정사각형) | stretch
    "pad": [0, 0, 0]
  },
  "input_name": null,                    // 없으면 모델의 첫 입력
  "license": { "name": "MIT", "url": "…" },
  "source": "https://…"
}
```

(실제 파일에는 주석을 쓰지 않습니다.)

- `face_detector`는 눈·코·입 다섯 점을 내야 얼굴을 바로 세워(112×112 템플릿) 특징값을 뽑을 수 있습니다. `yunet` 형식이 그렇습니다.
- `anime_detector`(`yolo` 형식)는 상자만 내고, 캐릭터 특징값은 얼굴 상자를 `analysis.character_crop_scale`배 키운 정사각형에서 뽑습니다.
- 임베더 출력은 길이 1로 정규화되어 코사인 유사도로 비교됩니다.

## 앱에서 받기

인물 화면의 **모델 받기**에서 아래 모델을 바로 받을 수 있습니다. 모델마다 라이선스 단계(전문 열기, 동의 체크)를 거쳐야 받기
버튼이 켜지고, 설정 폴더의 `models/<id>/`에 저장됩니다(동의 내용은 `consent.json`). 목록은 `catalog.json`이며 앱에 포함됩니다.
새 모델을 목록에 넣으려면 항목 하나(배포처 URL, 크기, 라이선스, 그대로 쓸 manifest)를 추가합니다.

## 예시

`examples/` 폴더에 아래 모델의 manifest 예시가 있습니다. 모델 파일을 받아 같은 폴더에 두고, 폴더째 모델 팩 위치에 복사하면 됩니다.
**파일 이름·입력 크기·전처리는 배포처의 최신 안내와 대조해 확인하세요.** 라이선스는 각 배포처에서 확인해야 합니다.

| 폴더 | 종류 | 배포처 | 비고 |
|---|---|---|---|
| `yunet` | 실사 얼굴 검출 | OpenCV Zoo `face_detection_yunet` | 상업 사용에 비교적 자유로운 라이선스 |
| `sface` | 실사 얼굴 특징값(128) | OpenCV Zoo `face_recognition_sface` | |
| `anime-face` | 애니 얼굴 검출 | Hugging Face `deepghs/anime_face_detection` | YOLOv8 계열 |
| `ccip` | 캐릭터 특징값 | Hugging Face `deepghs/ccip_onnx` | 특징값(`model_feat.onnx`)만 사용 |
| `wd-tagger` | 태거 | Hugging Face `SmilingWolf/wd-eva02-large-tagger-v3` (앱 목록에는 더 가벼운 `wd-vit-tagger-v3`) | `selected_tags.csv` 필요 |

InsightFace 계열(buffalo_l 등)은 정확도가 높지만 비상업 연구용 라이선스입니다. 개인적으로 쓰려면 같은 형식으로 manifest를
만들면 됩니다(검출기는 `yunet`과 출력이 달라 지금은 지원하지 않고, 특징값 모델(ArcFace, 112×112 RGB, `scale` 1/127.5,
`mean` 1)은 `embedding` 형식으로 쓸 수 있습니다).
