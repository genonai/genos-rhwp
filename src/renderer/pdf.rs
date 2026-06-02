//! PDF 렌더러 (Task #21)
//!
//! SVG 렌더러의 출력을 svg2pdf + pdf-writer로 PDF를 생성한다.
//! 단일/다중 페이지 모두 지원. 네이티브 전용 (WASM 미지원).

/// 폰트 데이터베이스를 초기화 (시스템 폰트 + 프로젝트 폰트 로드)
#[cfg(not(target_arch = "wasm32"))]
fn create_fontdb() -> usvg::fontdb::Database {
    let mut fontdb = usvg::fontdb::Database::new();
    fontdb.load_system_fonts();
    for dir in &["ttfs", "ttfs/windows", "ttfs/hwp"] {
        if std::path::Path::new(dir).exists() {
            fontdb.load_fonts_dir(dir);
        }
    }
    if std::path::Path::new("/mnt/c/Windows/Fonts").exists() {
        fontdb.load_fonts_dir("/mnt/c/Windows/Fonts");
    }
    #[cfg(target_os = "macos")]
    {
        fontdb.set_serif_family("AppleMyungjo");
        fontdb.set_sans_serif_family("Apple SD Gothic Neo");
        fontdb.set_monospace_family("Menlo");
    }
    #[cfg(not(target_os = "macos"))]
    {
        // 나눔 계열을 1순위로 — minimal Linux 환경에도 fonts-nanum 패키지로 기본 배포됨.
        // 시스템에 나눔이 없으면 chain 따라 Noto CJK 로 폴백됨.
        fontdb.set_serif_family("NanumMyeongjo");
        fontdb.set_sans_serif_family("NanumGothic");
        fontdb.set_monospace_family("D2Coding");
    }
    fontdb
}

#[cfg(not(target_arch = "wasm32"))]
fn pdf_sans_fallback() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "'Apple SD Gothic Neo','Apple Symbols','Arial Unicode MS','Noto Sans Symbols 2','Noto Sans Symbols','Symbola','AppleGothic','AppleMyungjo','Malgun Gothic','맑은 고딕','Noto Sans KR',sans-serif"
    }
    #[cfg(not(target_os = "macos"))]
    {
        // 나눔 계열 우선 (minimal Linux 호환성). Noto CJK 는 차순위 백업.
        "'나눔고딕','NanumGothic','나눔바른고딕','NanumBarunGothic','Noto Sans CJK KR','Noto Sans KR','Malgun Gothic','맑은 고딕','Segoe UI Symbol','Arial Unicode MS','Noto Sans Symbols 2','Noto Sans Symbols','Symbola','Apple SD Gothic Neo','AppleGothic',sans-serif"
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn pdf_serif_fallback() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "'AppleMyungjo','Apple Symbols','Arial Unicode MS','Noto Sans Symbols 2','Noto Sans Symbols','Symbola','Times New Roman',serif"
    }
    #[cfg(not(target_os = "macos"))]
    {
        // 나눔 계열 우선 (minimal Linux 호환성). Noto CJK / HCR 은 차순위 백업.
        "'나눔명조','NanumMyeongjo','함초롬바탕','HCR Batang','Noto Serif CJK KR','바탕','Batang','Segoe UI Symbol','Arial Unicode MS','Noto Sans Symbols 2','Noto Sans Symbols','Symbola','Times New Roman',serif"
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn pdf_mono_fallback() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "'Menlo','Courier New','D2Coding','Noto Sans Mono CJK KR',monospace"
    }
    #[cfg(not(target_os = "macos"))]
    {
        // D2Coding(나눔 계열 코드 폰트) 우선. fonts-nanum 패키지에 함께 배포됨.
        "'D2Coding','D2Coding ligature','나눔고딕코딩','NanumGothicCoding','Noto Sans Mono CJK KR','Courier New','DejaVu Sans Mono',monospace"
    }
}

/// SVG의 font-family에 fallback chain을 추가한다.
///
/// 하이브리드 전략 (mydocs/tech/font_fallback_strategy.md §3.2 / §5 참조):
///
/// 1단계 — enumeration: 알려진 HWP/MS/한컴/HY/휴먼 계열 폰트는 카테고리(Serif/Sans/Mono)별로
///         정확한 chain을 박는다. 시각적으로 명조→Serif, 고딕→Sans 매핑이 보존된다.
///
/// 2단계 — 정규식 폴백: 1단계에서 미처리된 단일 이름 font-family 는 이름 키워드
///         (명조/바탕/Myeongjo/Batang → Serif, 고딕/돋움/Gothic/Dotum → Sans,
///          Mono/Code → Mono) 로 카테고리를 추정해 chain을 박는다.
///
/// 3단계 — 디폴트: 카테고리 단서가 없는 폰트(영문 회사 폰트 등)는 Sans chain을 박는다.
///         시각적 손실이 가장 작은 안전한 디폴트.
#[cfg(not(target_arch = "wasm32"))]
fn add_font_fallbacks(svg: &str) -> String {
    // 1단계: enumeration — 알려진 폰트별 카테고리 매핑.
    // 키워드 추정만으로는 정확히 못 잡는 폰트 (예: "바탕체" → mono인지 serif인지)
    // 와 빈번히 등장하는 한글 폰트를 우선 명시.
    let mut s = svg.to_string();

    // generic family → OS별 chain
    s = s.replace(
        "font-family=\"sans-serif\"",
        &format!("font-family=\"{}\"", pdf_sans_fallback()),
    )
    .replace(
        "font-family=\"serif\"",
        &format!("font-family=\"{}\"", pdf_serif_fallback()),
    )
    .replace(
        "font-family=\"monospace\"",
        &format!("font-family=\"{}\"", pdf_mono_fallback()),
    );

    // Serif (명조/바탕 계열)
    for name in &[
        "휴먼명조", "한컴바탕", "새바탕", "함초롬바탕", "함초롱바탕",
        "바탕", "Batang", "HY신명조", "HY견명조", "궁서", "새궁서",
    ] {
        let from = format!("font-family=\"{}\"", name);
        let to = format!("font-family=\"{}, {}\"", name, pdf_serif_fallback());
        s = s.replace(&from, &to);
    }

    // Monospace ("체" 가 붙은 Windows 고정폭 폰트 — Sans보다 먼저 처리)
    for name in &[
        "바탕체", "BatangChe", "돋움체", "DotumChe",
        "굴림체", "GulimChe", "궁서체", "GungsuhChe",
    ] {
        let from = format!("font-family=\"{}\"", name);
        let to = format!("font-family=\"{}, {}\"", name, pdf_mono_fallback());
        s = s.replace(&from, &to);
    }

    // Sans-serif (고딕/돋움/굴림 계열)
    for name in &[
        "휴먼고딕", "휴먼 고딕", "한컴돋움", "새돋움",
        "함초롬돋움", "함초롱돋움", "돋움", "Dotum",
        "굴림", "Gulim", "새굴림",
        "맑은 고딕", "Malgun Gothic",
        "HY중고딕", "HY견고딕", "HY헤드라인M", "HY그래픽",
        "HCI Poppy",
    ] {
        let from = format!("font-family=\"{}\"", name);
        let to = format!("font-family=\"{}, {}\"", name, pdf_sans_fallback());
        s = s.replace(&from, &to);
    }

    // 2단계 + 3단계: enumeration 에서 못 잡은 font-family 정규식 폴백.
    // `font-family="X"` 패턴에서 X 안에 콤마가 없는 것 (=chain 미박힘) 만 매칭.
    // 컴파일은 호출당 1회 — PDF 변환은 빈번하지 않으므로 static 캐시 불필요.
    let re = regex::Regex::new(r#"font-family="([^",]+)""#).unwrap();
    re.replace_all(&s, |caps: &regex::Captures| {
        let name = &caps[1];
        let lower = name.to_lowercase();

        // 2단계: 키워드 기반 카테고리 추정
        let chain = if name.contains("명조") || name.contains("바탕") || name.contains("궁서")
            || lower.contains("myeongjo") || lower.contains("batang")
            || lower.contains("serif")
        {
            pdf_serif_fallback()
        } else if lower.contains("mono")
            || lower.contains("coding")
            || lower.ends_with(" code")
            || lower == "code"
        {
            pdf_mono_fallback()
        } else if name.contains("고딕") || name.contains("돋움") || name.contains("굴림")
            || lower.contains("gothic") || lower.contains("dotum") || lower.contains("gulim")
            || lower.contains("sans")
        {
            pdf_sans_fallback()
        } else {
            // 3단계: 카테고리 단서 없음 → 안전한 Sans 디폴트
            pdf_sans_fallback()
        };
        format!(r#"font-family="{}, {}""#, name, chain)
    })
    .into_owned()
}

/// 단일 SVG를 PDF로 변환
#[cfg(not(target_arch = "wasm32"))]
pub fn svg_to_pdf(svg_content: &str) -> Result<Vec<u8>, String> {
    let fontdb = create_fontdb();
    let mut options = usvg::Options::default();
    options.fontdb = std::sync::Arc::new(fontdb);
    let svg_with_fallback = add_font_fallbacks(svg_content);
    let tree = usvg::Tree::from_str(&svg_with_fallback, &options)
        .map_err(|e| format!("SVG 파싱 실패: {}", e))?;
    let pdf = svg2pdf::to_pdf(&tree, svg2pdf::ConversionOptions::default(), svg2pdf::PageOptions::default())
        .map_err(|e| format!("PDF 변환 실패: {:?}", e))?;
    Ok(pdf)
}

/// 여러 SVG 페이지를 단일 다중 페이지 PDF로 생성
#[cfg(not(target_arch = "wasm32"))]
pub fn svgs_to_pdf(svg_pages: &[String]) -> Result<Vec<u8>, String> {
    if svg_pages.is_empty() {
        return Err("페이지가 없습니다".to_string());
    }
    if svg_pages.len() == 1 {
        return svg_to_pdf(&svg_pages[0]);
    }

    use pdf_writer::{Pdf, Ref, Finish};
    use std::collections::HashMap;

    let fontdb = create_fontdb();
    let mut options = usvg::Options::default();
    options.fontdb = std::sync::Arc::new(fontdb);

    let mut alloc = Ref::new(1);
    let catalog_ref = alloc.bump();
    let page_tree_ref = alloc.bump();

    // 각 페이지의 SVG를 파싱하여 chunk + page 정보 수집
    struct PageData {
        chunk: pdf_writer::Chunk,
        svg_ref: Ref,
        width: f32,
        height: f32,
    }

    let mut page_datas: Vec<PageData> = Vec::new();

    for svg in svg_pages {
        let svg_with_fallback = add_font_fallbacks(svg);
        let tree = usvg::Tree::from_str(&svg_with_fallback, &options)
            .map_err(|e| format!("SVG 파싱 실패: {}", e))?;

        let (chunk, svg_ref) = svg2pdf::to_chunk(&tree, svg2pdf::ConversionOptions::default())
            .map_err(|e| format!("SVG→chunk 변환 실패: {:?}", e))?;

        let dpi_ratio = 72.0 / 96.0; // 96 DPI → 72 pt
        let w = tree.size().width() * dpi_ratio;
        let h = tree.size().height() * dpi_ratio;

        page_datas.push(PageData { chunk, svg_ref, width: w, height: h });
    }

    // 각 chunk를 재번호화하고 페이지 참조 수집
    let mut page_refs: Vec<Ref> = Vec::new();
    let mut renumbered_chunks: Vec<pdf_writer::Chunk> = Vec::new();
    let mut svg_refs_remapped: Vec<Ref> = Vec::new();

    for pd in &page_datas {
        let page_ref = alloc.bump();
        let content_ref = alloc.bump();
        page_refs.push(page_ref);

        // chunk 재번호화
        let mut map = HashMap::new();
        let renumbered = pd.chunk.renumber(|old| {
            *map.entry(old).or_insert_with(|| alloc.bump())
        });

        let remapped_svg_ref = map.get(&pd.svg_ref).copied().unwrap_or(pd.svg_ref);
        svg_refs_remapped.push(remapped_svg_ref);
        renumbered_chunks.push(renumbered);
    }

    // PDF 생성
    let mut pdf = Pdf::new();
    pdf.catalog(catalog_ref).pages(page_tree_ref);
    pdf.pages(page_tree_ref)
        .count(page_refs.len() as i32)
        .kids(page_refs.iter().copied());

    // 각 페이지 생성
    let svg_name = pdf_writer::Name(b"S1");

    for (i, pd) in page_datas.iter().enumerate() {
        let page_ref = page_refs[i];
        let content_ref = alloc.bump();
        let svg_ref = svg_refs_remapped[i];

        let mut page = pdf.page(page_ref);
        page.media_box(pdf_writer::Rect::new(0.0, 0.0, pd.width, pd.height));
        page.parent(page_tree_ref);
        page.contents(content_ref);

        let mut resources = page.resources();
        resources.x_objects().pair(svg_name, svg_ref);
        resources.finish();
        page.finish();

        // 컨텐츠 스트림: SVG XObject를 페이지 크기에 맞게 배치
        let mut content = pdf_writer::Content::new();
        content.transform([pd.width, 0.0, 0.0, pd.height, 0.0, 0.0]);
        content.x_object(svg_name);

        pdf.stream(content_ref, &content.finish());
    }

    // 모든 chunk를 PDF에 추가
    for chunk in &renumbered_chunks {
        pdf.extend(chunk);
    }

    // 문서 정보
    let info_ref = alloc.bump();
    pdf.document_info(info_ref).producer(pdf_writer::TextStr("rhwp"));

    Ok(pdf.finish())
}
