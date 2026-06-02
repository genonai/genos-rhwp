//! HWPML / OWPML 단일 XML 문서 파서
//!
//! - 신형 OWPML 계열: `<head>` / `<sec>` 서브트리를 추출해 기존 HWPX 파서를 재사용
//! - 구형 HWPML 2.x 계열: `<HWPML><HEAD>...<BODY><SECTION>...` 구조를 직접 파싱

use base64::Engine;
use encoding_rs::{UTF_16BE, UTF_16LE, UTF_8};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};

use crate::model::bin_data::{BinData, BinDataContent, BinDataType};
use crate::model::control::{AutoNumber, AutoNumberType, Bookmark, Control};
use crate::model::document::{
    DocInfo, DocProperties, Document, FileHeader, HwpVersion, Section, SectionDef,
};
use crate::model::header_footer::{Footer, Header, HeaderFooterApply};
use crate::model::image::{CropInfo, ImageEffect, Picture};
use crate::model::page::{BindingMethod, PageDef};
use crate::model::paragraph::{CharShapeRef, ColumnBreakType, LineSeg, Paragraph};
use crate::model::shape::{
    CommonObjAttr, HorzAlign, HorzRelTo, ShapeComponentAttr, TextWrap, VertAlign, VertRelTo,
};
use crate::model::style::{
    Alignment, BorderFill, BorderLineType, CharShape, FillType, Font, HeadType,
    LineSpacingType, Numbering, NumberingHead, ParaShape, SolidFill, Style, TabDef, TabItem,
    UnderlineType,
};
use crate::model::table::{Cell, Table, TablePageBreak, VerticalAlign};

use super::hwpx;

/// HWPML 파싱 에러는 기존 HWPX XML 에러 타입을 재사용한다.
pub type HwpmlError = hwpx::HwpxError;

pub fn parse_hwpml(data: &[u8]) -> Result<Document, HwpmlError> {
    let xml = decode_xml(data)?;
    if is_legacy_hwpml(&xml) {
        parse_legacy_hwpml(&xml)
    } else {
        parse_modern_hwpml(&xml)
    }
}

fn is_legacy_hwpml(xml: &str) -> bool {
    xml.contains("<HWPML") && (xml.contains("<BODY") || xml.contains("<SECTION"))
}

fn parse_modern_hwpml(xml: &str) -> Result<Document, HwpmlError> {
    let header_xml = extract_first_subtree(xml, b"head")?
        .ok_or_else(|| HwpmlError::MissingFile("HWPML head element".to_string()))?;
    let section_xmls = extract_all_subtrees(xml, b"sec")?;
    if section_xmls.is_empty() {
        return Err(HwpmlError::MissingFile("HWPML sec element".to_string()));
    }

    let (doc_info, mut doc_properties) = hwpx::header::parse_hwpx_header(&header_xml)?;
    let mut sections = Vec::new();
    for section_xml in &section_xmls {
        match hwpx::section::parse_hwpx_section(section_xml) {
            Ok(section) => sections.push(section),
            Err(e) => {
                eprintln!("경고: HWPML section 파싱 실패: {}", e);
                sections.push(Section::default());
            }
        }
    }
    doc_properties.section_count = sections.len() as u16;
    Ok(build_document(
        HwpVersion { major: 3, minor: 0, build: 0, revision: 0 },
        doc_properties,
        doc_info,
        sections,
        Vec::new(),
    ))
}

fn parse_legacy_hwpml(xml: &str) -> Result<Document, HwpmlError> {
    let (doc_info, mut doc_properties, bin_data_content) = parse_legacy_header(xml)?;
    let sections = parse_legacy_body(xml)?;
    doc_properties.section_count = sections.len() as u16;
    Ok(build_document(
        HwpVersion { major: 2, minor: 1, build: 0, revision: 0 },
        doc_properties,
        doc_info,
        sections,
        bin_data_content,
    ))
}

fn build_document(
    version: HwpVersion,
    doc_properties: DocProperties,
    doc_info: DocInfo,
    sections: Vec<Section>,
    bin_data_content: Vec<BinDataContent>,
) -> Document {
    Document {
        header: FileHeader {
            version,
            flags: 0,
            compressed: false,
            encrypted: false,
            distribution: false,
            raw_data: None,
        },
        doc_properties,
        doc_info,
        sections,
        preview: None,
        bin_data_content,
        extra_streams: Vec::new(),
        // HWPML(모던) 은 HWP3-origin 변환본이 아니므로 false (v0.7.13 신규 필드).
        is_hwp3_variant: false,
    }
}

fn parse_legacy_header(xml: &str) -> Result<(DocInfo, DocProperties, Vec<BinDataContent>), HwpmlError> {
    let mut doc_info = DocInfo {
        font_faces: vec![Vec::new(); 7],
        ..Default::default()
    };
    let mut doc_props = DocProperties::default();
    let mut bin_data_content = Vec::new();

    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut current_font_group = 0usize;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                if eq_name(local, b"FONTFACE") {
                    current_font_group = legacy_font_group(e);
                } else if eq_name(local, b"FONT") {
                    parse_legacy_font(e, &mut doc_info, current_font_group);
                } else if eq_name(local, b"BEGINNUMBER") {
                    parse_legacy_begin_number(e, &mut doc_props);
                } else if eq_name(local, b"CARETPOS") {
                    parse_legacy_caret_pos(e, &mut doc_props);
                } else if eq_name(local, b"BINITEM") {
                    parse_legacy_bin_item(e, &mut doc_info);
                } else if eq_name(local, b"BORDERFILL") {
                    parse_legacy_border_fill(e, &mut reader, &mut doc_info)?;
                } else if eq_name(local, b"CHARSHAPE") {
                    parse_legacy_char_shape(e, &mut reader, &mut doc_info)?;
                } else if eq_name(local, b"TABDEF") {
                    parse_legacy_tab_def(e, &mut reader, &mut doc_info)?;
                } else if eq_name(local, b"NUMBERING") {
                    parse_legacy_numbering(e, &mut reader, &mut doc_info)?;
                } else if eq_name(local, b"PARASHAPE") {
                    parse_legacy_para_shape(e, &mut reader, &mut doc_info)?;
                } else if eq_name(local, b"STYLE") {
                    parse_legacy_style(e, &mut doc_info);
                    skip_element(&mut reader, b"STYLE")?;
                } else if eq_name(local, b"BINDATA") {
                    parse_legacy_bindata(e, &mut reader, &mut bin_data_content)?;
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                if eq_name(local, b"FONT") {
                    parse_legacy_font(e, &mut doc_info, current_font_group);
                } else if eq_name(local, b"BEGINNUMBER") {
                    parse_legacy_begin_number(e, &mut doc_props);
                } else if eq_name(local, b"CARETPOS") {
                    parse_legacy_caret_pos(e, &mut doc_props);
                } else if eq_name(local, b"BINITEM") {
                    parse_legacy_bin_item(e, &mut doc_info);
                } else if eq_name(local, b"TABDEF") {
                    parse_legacy_tab_def_empty(e, &mut doc_info);
                } else if eq_name(local, b"STYLE") {
                    parse_legacy_style(e, &mut doc_info);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy HWPML header: {}", e))),
            _ => {}
        }
        buf.clear();
    }

    supplement_legacy_doc_info(xml, &mut doc_info)?;

    Ok((doc_info, doc_props, bin_data_content))
}

fn supplement_legacy_doc_info(xml: &str, doc_info: &mut DocInfo) -> Result<(), HwpmlError> {
    scan_legacy_elements(xml, b"BORDERFILL", |e, reader, is_empty| {
        if !is_empty {
            parse_legacy_border_fill(e, reader, doc_info)?;
        }
        Ok(())
    })?;
    scan_legacy_elements(xml, b"CHARSHAPE", |e, reader, is_empty| {
        if !is_empty {
            parse_legacy_char_shape(e, reader, doc_info)?;
        }
        Ok(())
    })?;
    scan_legacy_elements(xml, b"TABDEF", |e, reader, is_empty| {
        if is_empty {
            parse_legacy_tab_def_empty(e, doc_info);
        } else {
            parse_legacy_tab_def(e, reader, doc_info)?;
        }
        Ok(())
    })?;
    scan_legacy_elements(xml, b"NUMBERING", |e, reader, is_empty| {
        if !is_empty {
            parse_legacy_numbering(e, reader, doc_info)?;
        }
        Ok(())
    })?;
    scan_legacy_elements(xml, b"PARASHAPE", |e, reader, is_empty| {
        if !is_empty {
            parse_legacy_para_shape(e, reader, doc_info)?;
        }
        Ok(())
    })?;
    scan_legacy_elements(xml, b"STYLE", |e, reader, is_empty| {
        parse_legacy_style(e, doc_info);
        if !is_empty {
            skip_element(reader, b"STYLE")?;
        }
        Ok(())
    })?;
    Ok(())
}

fn scan_legacy_elements<F>(xml: &str, target_name: &[u8], mut on_match: F) -> Result<(), HwpmlError>
where
    F: FnMut(&BytesStart, &mut Reader<&[u8]>, bool) -> Result<(), HwpmlError>,
{
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if eq_name(local_name(e.name().as_ref()), target_name) => {
                on_match(e, &mut reader, false)?;
            }
            Ok(Event::Empty(ref e)) if eq_name(local_name(e.name().as_ref()), target_name) => {
                on_match(e, &mut reader, true)?;
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(HwpmlError::XmlError(format!(
                    "legacy HWPML scan {}: {}",
                    String::from_utf8_lossy(target_name),
                    e
                )))
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn parse_legacy_body(xml: &str) -> Result<Vec<Section>, HwpmlError> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut sections = Vec::new();
    let mut current_section: Option<Section> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = local_name(e.name().as_ref()).to_ascii_uppercase();
                if local.as_slice() == b"SECTION" {
                    current_section = Some(Section::default());
                } else if local.as_slice() == b"P" && current_section.is_some() {
                    if let Some(section) = &mut current_section {
                        let (para, sec_def_opt) = parse_legacy_paragraph(e, &mut reader)?;
                        if let Some(sec_def) = sec_def_opt {
                            section.section_def = sec_def;
                        }
                        section.paragraphs.push(para);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name().as_ref()).to_ascii_uppercase();
                if local.as_slice() == b"SECTION" {
                    if let Some(section) = current_section.take() {
                        sections.push(section);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy HWPML body: {}", e))),
            _ => {}
        }
        buf.clear();
    }

    if sections.is_empty() {
        return Err(HwpmlError::MissingFile("legacy HWPML SECTION".to_string()));
    }
    Ok(sections)
}

fn parse_legacy_paragraph(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
) -> Result<(Paragraph, Option<SectionDef>), HwpmlError> {
    let mut para = Paragraph::default();
    let mut sec_def = None;

    if let Some(v) = attr_u16(e, b"ParaShape") {
        para.para_shape_id = v;
    }
    if let Some(v) = attr_u8(e, b"Style") {
        para.style_id = v;
    }
    if attr_bool(e, b"PageBreak") {
        para.column_type = ColumnBreakType::Page;
    }

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"TEXT") => {
                let char_shape_id = attr_u32(ce, b"CharShape").unwrap_or(0);
                append_char_shape_change(&mut para, char_shape_id);
                parse_legacy_text_run(ce, reader, &mut para, &mut sec_def)?;
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"TEXT") => {
                let char_shape_id = attr_u32(ce, b"CharShape").unwrap_or(0);
                append_char_shape_change(&mut para, char_shape_id);
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"P") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy paragraph: {}", e))),
            _ => {}
        }
        buf.clear();
    }

    finalize_paragraph(&mut para);
    Ok((para, sec_def))
}

fn parse_legacy_text_run(
    _e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    para: &mut Paragraph,
    sec_def: &mut Option<SectionDef>,
) -> Result<(), HwpmlError> {
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"CHAR") => {
                let text = read_plain_text(reader, b"CHAR")?;
                push_text(para, &normalize_text(&text));
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"CHAR") => {}
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"SECDEF") => {
                *sec_def = Some(parse_legacy_secdef(ce, reader)?);
            }
            Ok(Event::Start(ref ce)) => {
                let local = local_name(ce.name().as_ref()).to_ascii_uppercase();
                if local.as_slice() == b"LINEBREAK" {
                    push_text(para, "\n");
                    skip_element(reader, b"LINEBREAK")?;
                } else if local.as_slice() == b"NBSPACE" {
                    push_text(para, " ");
                    skip_element(reader, b"NBSPACE")?;
                } else if local.as_slice() == b"COLDEF" {
                    let coldef = parse_legacy_coldef(ce, reader)?;
                    para.controls.push(Control::ColumnDef(coldef));
                } else if local.as_slice() == b"PICTURE" {
                    let picture = parse_legacy_picture(ce, reader)?;
                    para.controls.push(Control::Picture(Box::new(picture)));
                } else if local.as_slice() == b"TABLE" {
                    let table = parse_legacy_table(ce, reader)?;
                    para.controls.push(Control::Table(Box::new(table)));
                } else if local.as_slice() == b"HEADER" {
                    let header = parse_legacy_header_ctrl(ce, reader)?;
                    para.controls.push(Control::Header(Box::new(header)));
                } else if local.as_slice() == b"FOOTER" {
                    let footer = parse_legacy_footer_ctrl(ce, reader)?;
                    para.controls.push(Control::Footer(Box::new(footer)));
                } else if local.as_slice() == b"FIELDBEGIN" {
                    if let Some(bookmark) = parse_legacy_bookmark(ce) {
                        para.controls.push(Control::Bookmark(bookmark));
                    }
                    skip_element(reader, b"FIELDBEGIN")?;
                } else if local.as_slice() == b"AUTONUM" {
                    let an = parse_legacy_autonum(ce);
                    para.controls.push(Control::AutoNumber(an));
                    skip_element(reader, b"AUTONUM")?;
                } else {
                    skip_element(reader, local.as_slice())?;
                }
            }
            Ok(Event::Empty(ref ce)) => {
                let local = local_name(ce.name().as_ref()).to_ascii_uppercase();
                if local.as_slice() == b"LINEBREAK" {
                    push_text(para, "\n");
                } else if local.as_slice() == b"NBSPACE" {
                    push_text(para, " ");
                } else if local.as_slice() == b"COLDEF" {
                    let coldef = parse_legacy_coldef_empty(ce);
                    para.controls.push(Control::ColumnDef(coldef));
                } else if local.as_slice() == b"PICTURE" {
                    para.controls.push(Control::Picture(Box::new(Picture::default())));
                } else if local.as_slice() == b"FIELDBEGIN" {
                    if let Some(bookmark) = parse_legacy_bookmark(ce) {
                        para.controls.push(Control::Bookmark(bookmark));
                    }
                } else if local.as_slice() == b"AUTONUM" {
                    let an = parse_legacy_autonum(ce);
                    para.controls.push(Control::AutoNumber(an));
                }
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"TEXT") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy TEXT: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn parse_legacy_coldef(e: &BytesStart, reader: &mut Reader<&[u8]>) -> Result<crate::model::page::ColumnDef, HwpmlError> {
    let coldef = parse_legacy_coldef_empty(e);
    skip_element(reader, b"COLDEF")?;
    Ok(coldef)
}

fn parse_legacy_coldef_empty(e: &BytesStart) -> crate::model::page::ColumnDef {
    let mut coldef = crate::model::page::ColumnDef {
        column_count: attr_u16(e, b"Count").unwrap_or(1).max(1),
        same_width: attr_bool(e, b"SameSize"),
        spacing: attr_i16(e, b"SameGap").or_else(|| attr_i16(e, b"Space")).unwrap_or(0),
        ..Default::default()
    };
    coldef.column_type = match attr_string(e, b"Type").as_deref() {
        Some("Distribute") => crate::model::page::ColumnType::Distribute,
        Some("Parallel") => crate::model::page::ColumnType::Parallel,
        _ => crate::model::page::ColumnType::Normal,
    };
    coldef.direction = match attr_string(e, b"Layout").as_deref() {
        Some("Right") | Some("RightToLeft") => crate::model::page::ColumnDirection::RightToLeft,
        _ => crate::model::page::ColumnDirection::LeftToRight,
    };
    coldef
}

fn parse_legacy_header_ctrl(e: &BytesStart, reader: &mut Reader<&[u8]>) -> Result<Header, HwpmlError> {
    let mut header = Header {
        apply_to: parse_legacy_apply_page_type(attr_string(e, b"ApplyPageType").as_deref()),
        ..Default::default()
    };
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARALIST") => {
                header.paragraphs = parse_legacy_paralist(reader, b"PARALIST")?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"HEADER") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy HEADER: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(header)
}

fn parse_legacy_footer_ctrl(e: &BytesStart, reader: &mut Reader<&[u8]>) -> Result<Footer, HwpmlError> {
    let mut footer = Footer {
        apply_to: parse_legacy_apply_page_type(attr_string(e, b"ApplyPageType").as_deref()),
        ..Default::default()
    };
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARALIST") => {
                footer.paragraphs = parse_legacy_paralist(reader, b"PARALIST")?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"FOOTER") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy FOOTER: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(footer)
}

fn parse_legacy_paralist(reader: &mut Reader<&[u8]>, end_tag: &[u8]) -> Result<Vec<Paragraph>, HwpmlError> {
    let mut paragraphs = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"P") => {
                let (para, _) = parse_legacy_paragraph(ce, reader)?;
                paragraphs.push(para);
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), end_tag) => break,
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(HwpmlError::XmlError(format!(
                    "legacy PARALIST {}: {}",
                    String::from_utf8_lossy(end_tag),
                    e
                )))
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(paragraphs)
}

fn parse_legacy_picture(_e: &BytesStart, reader: &mut Reader<&[u8]>) -> Result<Picture, HwpmlError> {
    let mut pic = Picture::default();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"SHAPEOBJECT") => {
                parse_legacy_shape_object(ce, reader, &mut pic.common)?;
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"SHAPECOMPONENT") => {
                parse_legacy_shape_component_attrs(ce, &mut pic.shape_attr);
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"SHAPECOMPONENT") => {
                parse_legacy_shape_component(ce, reader, &mut pic.shape_attr)?;
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"IMAGECLIP") => {
                pic.crop.left = attr_i32(ce, b"Left").unwrap_or(0);
                pic.crop.top = attr_i32(ce, b"Top").unwrap_or(0);
                pic.crop.right = attr_i32(ce, b"Right").unwrap_or(0);
                pic.crop.bottom = attr_i32(ce, b"Bottom").unwrap_or(0);
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"INSIDEMARGIN") => {
                pic.padding.left = attr_i16(ce, b"Left").unwrap_or(0);
                pic.padding.right = attr_i16(ce, b"Right").unwrap_or(0);
                pic.padding.top = attr_i16(ce, b"Top").unwrap_or(0);
                pic.padding.bottom = attr_i16(ce, b"Bottom").unwrap_or(0);
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"IMAGE") => {
                pic.image_attr.brightness = attr_i8(ce, b"Bright").unwrap_or(0);
                pic.image_attr.contrast = attr_i8(ce, b"Contrast").unwrap_or(0);
                pic.image_attr.effect = match attr_string(ce, b"Effect").as_deref() {
                    Some("GrayScale") => ImageEffect::GrayScale,
                    Some("BlackWhite") => ImageEffect::BlackWhite,
                    _ => ImageEffect::RealPic,
                };
                pic.image_attr.bin_data_id = attr_u16(ce, b"BinItem").unwrap_or(0);
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"PICTURE") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy PICTURE: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(pic)
}

fn parse_legacy_table(e: &BytesStart, reader: &mut Reader<&[u8]>) -> Result<Table, HwpmlError> {
    let mut table = Table {
        row_count: attr_u16(e, b"RowCount").unwrap_or(0),
        col_count: attr_u16(e, b"ColCount").unwrap_or(0),
        cell_spacing: attr_i16(e, b"CellSpacing").unwrap_or(0),
        border_fill_id: attr_u16(e, b"BorderFill").unwrap_or(0),
        repeat_header: attr_bool(e, b"RepeatHeader"),
        page_break: match attr_string(e, b"PageBreak").as_deref() {
            Some("Cell") => TablePageBreak::CellBreak,
            Some("Row") => TablePageBreak::RowBreak,
            _ => TablePageBreak::None,
        },
        ..Default::default()
    };

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"SHAPEOBJECT") => {
                parse_legacy_shape_object(ce, reader, &mut table.common)?;
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"INSIDEMARGIN") => {
                table.padding.left = attr_i16(ce, b"Left").unwrap_or(0);
                table.padding.right = attr_i16(ce, b"Right").unwrap_or(0);
                table.padding.top = attr_i16(ce, b"Top").unwrap_or(0);
                table.padding.bottom = attr_i16(ce, b"Bottom").unwrap_or(0);
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"ROW") => {
                parse_legacy_table_row(reader, &mut table)?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"TABLE") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy TABLE: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    if table.row_count == 0 {
        table.row_count = table
            .cells
            .iter()
            .map(|c| c.row.saturating_add(c.row_span))
            .max()
            .unwrap_or(0);
    }
    if table.col_count == 0 {
        table.col_count = table
            .cells
            .iter()
            .map(|c| c.col.saturating_add(c.col_span))
            .max()
            .unwrap_or(0);
    }
    table.rebuild_grid();
    Ok(table)
}

fn parse_legacy_table_row(reader: &mut Reader<&[u8]>, table: &mut Table) -> Result<(), HwpmlError> {
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"CELL") => {
                let cell = parse_legacy_table_cell(ce, reader)?;
                table.cells.push(cell);
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"ROW") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy ROW: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn parse_legacy_table_cell(e: &BytesStart, reader: &mut Reader<&[u8]>) -> Result<Cell, HwpmlError> {
    let mut cell = Cell {
        col: attr_u16(e, b"ColAddr").unwrap_or(0),
        row: attr_u16(e, b"RowAddr").unwrap_or(0),
        col_span: attr_u16(e, b"ColSpan").unwrap_or(1).max(1),
        row_span: attr_u16(e, b"RowSpan").unwrap_or(1).max(1),
        width: attr_u32(e, b"Width").unwrap_or(0),
        height: attr_u32(e, b"Height").unwrap_or(0),
        border_fill_id: attr_u16(e, b"BorderFill").unwrap_or(0),
        is_header: attr_bool(e, b"Header"),
        apply_inner_margin: attr_bool(e, b"HasMargin"),
        ..Default::default()
    };

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARALIST") => {
                cell.paragraphs = parse_legacy_paralist(reader, b"PARALIST")?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"CELL") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy CELL: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    if cell.paragraphs.is_empty() {
        cell.paragraphs.push(Paragraph::new_empty());
    }
    Ok(cell)
}

fn parse_legacy_shape_object(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    common: &mut CommonObjAttr,
) -> Result<(), HwpmlError> {
    common.instance_id = attr_u32(e, b"InstId").unwrap_or(0);
    common.z_order = attr_i32(e, b"ZOrder").unwrap_or(0);
    let wrap = attr_string(e, b"TextWrap");
    common.text_wrap = parse_legacy_text_wrap(wrap.as_deref());

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"SIZE") => {
                common.width = attr_u32(ce, b"Width").unwrap_or(common.width);
                common.height = attr_u32(ce, b"Height").unwrap_or(common.height);
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"POSITION") => {
                common.treat_as_char = attr_bool(ce, b"TreatAsChar");
                common.vert_rel_to = parse_legacy_vert_rel(attr_string(ce, b"VertRelTo").as_deref());
                common.horz_rel_to = parse_legacy_horz_rel(attr_string(ce, b"HorzRelTo").as_deref());
                common.vert_align = parse_legacy_vert_align(attr_string(ce, b"VertAlign").as_deref());
                common.horz_align = parse_legacy_horz_align(attr_string(ce, b"HorzAlign").as_deref());
                common.vertical_offset = attr_i32(ce, b"VertOffset").unwrap_or(0) as u32;
                common.horizontal_offset = attr_i32(ce, b"HorzOffset").unwrap_or(0) as u32;
                if matches!(attr_string(ce, b"FlowWithText").as_deref(), Some("false") | Some("False") | Some("0")) {
                    common.text_wrap = parse_legacy_text_wrap(attr_string(e, b"TextWrap").as_deref());
                }
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"OUTSIDEMARGIN") => {
                common.margin.left = attr_i16(ce, b"Left").unwrap_or(0);
                common.margin.right = attr_i16(ce, b"Right").unwrap_or(0);
                common.margin.top = attr_i16(ce, b"Top").unwrap_or(0);
                common.margin.bottom = attr_i16(ce, b"Bottom").unwrap_or(0);
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"SHAPEOBJECT") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy SHAPEOBJECT: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn parse_legacy_shape_component_attrs(e: &BytesStart, shape_attr: &mut ShapeComponentAttr) {
    shape_attr.offset_x = attr_i32(e, b"XPos").unwrap_or(0);
    shape_attr.offset_y = attr_i32(e, b"YPos").unwrap_or(0);
    shape_attr.group_level = attr_u16(e, b"GroupLevel").unwrap_or(0);
    shape_attr.original_width = attr_u32(e, b"OriWidth").unwrap_or(0);
    shape_attr.original_height = attr_u32(e, b"OriHeight").unwrap_or(0);
    shape_attr.current_width = attr_u32(e, b"CurWidth").unwrap_or(0);
    shape_attr.current_height = attr_u32(e, b"CurHeight").unwrap_or(0);
    shape_attr.horz_flip = attr_bool(e, b"HorzFlip");
    shape_attr.vert_flip = attr_bool(e, b"VertFlip");
}

fn parse_legacy_shape_component(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    shape_attr: &mut ShapeComponentAttr,
) -> Result<(), HwpmlError> {
    parse_legacy_shape_component_attrs(e, shape_attr);
    skip_element(reader, b"SHAPECOMPONENT")?;
    Ok(())
}

fn parse_legacy_bookmark(e: &BytesStart) -> Option<Bookmark> {
    if !matches!(attr_string(e, b"Type").as_deref(), Some("Bookmark")) {
        return None;
    }
    Some(Bookmark {
        name: attr_string(e, b"Name").unwrap_or_default(),
    })
}

fn parse_legacy_autonum(e: &BytesStart) -> AutoNumber {
    AutoNumber {
        number_type: parse_legacy_num_type(attr_string(e, b"NumberType").as_deref()),
        number: attr_u16(e, b"Number").unwrap_or(0),
        assigned_number: attr_u16(e, b"Number").unwrap_or(0),
        ..Default::default()
    }
}

fn parse_legacy_secdef(e: &BytesStart, reader: &mut Reader<&[u8]>) -> Result<SectionDef, HwpmlError> {
    let mut sec = SectionDef::default();
    sec.column_spacing = attr_i16(e, b"SpaceColumns").unwrap_or(0);
    sec.default_tab_spacing = attr_u32(e, b"TabStop").unwrap_or(0);
    sec.outline_numbering_id = attr_u16(e, b"OutlineShape").unwrap_or(0);

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"STARTNUMBER") => {
                sec.page_num_type = match attr_string(ce, b"PageStartsOn").as_deref() {
                    Some("Odd") => 1,
                    Some("Even") => 2,
                    _ => 0,
                };
                sec.page_num = attr_u16(ce, b"Page").unwrap_or(0);
                sec.picture_num = attr_u16(ce, b"Figure").unwrap_or(0);
                sec.table_num = attr_u16(ce, b"Table").unwrap_or(0);
                sec.equation_num = attr_u16(ce, b"Equation").unwrap_or(0);
                skip_element(reader, b"STARTNUMBER")?;
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"STARTNUMBER") => {
                sec.page_num_type = match attr_string(ce, b"PageStartsOn").as_deref() {
                    Some("Odd") => 1,
                    Some("Even") => 2,
                    _ => 0,
                };
                sec.page_num = attr_u16(ce, b"Page").unwrap_or(0);
                sec.picture_num = attr_u16(ce, b"Figure").unwrap_or(0);
                sec.table_num = attr_u16(ce, b"Table").unwrap_or(0);
                sec.equation_num = attr_u16(ce, b"Equation").unwrap_or(0);
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PAGEDEF") => {
                parse_legacy_page_def(ce, reader, &mut sec.page_def)?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"SECDEF") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy SECDEF: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(sec)
}

fn parse_legacy_page_def(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    page: &mut PageDef,
) -> Result<(), HwpmlError> {
    page.width = attr_u32(e, b"Width").unwrap_or(0);
    page.height = attr_u32(e, b"Height").unwrap_or(0);
    page.landscape = matches!(
        attr_string(e, b"Landscape").as_deref(),
        Some("1") | Some("true") | Some("True")
    );
    page.binding = match attr_string(e, b"GutterType").as_deref() {
        Some("TopOnly") => BindingMethod::TopFlip,
        Some("Both") => BindingMethod::DuplexSided,
        _ => BindingMethod::SingleSided,
    };

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PAGEMARGIN") => {
                page.margin_left = attr_u32(ce, b"Left").unwrap_or(0);
                page.margin_right = attr_u32(ce, b"Right").unwrap_or(0);
                page.margin_top = attr_u32(ce, b"Top").unwrap_or(0);
                page.margin_bottom = attr_u32(ce, b"Bottom").unwrap_or(0);
                page.margin_header = attr_u32(ce, b"Header").unwrap_or(0);
                page.margin_footer = attr_u32(ce, b"Footer").unwrap_or(0);
                page.margin_gutter = attr_u32(ce, b"Gutter").unwrap_or(0);
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PAGEMARGIN") => {
                page.margin_left = attr_u32(ce, b"Left").unwrap_or(0);
                page.margin_right = attr_u32(ce, b"Right").unwrap_or(0);
                page.margin_top = attr_u32(ce, b"Top").unwrap_or(0);
                page.margin_bottom = attr_u32(ce, b"Bottom").unwrap_or(0);
                page.margin_header = attr_u32(ce, b"Header").unwrap_or(0);
                page.margin_footer = attr_u32(ce, b"Footer").unwrap_or(0);
                page.margin_gutter = attr_u32(ce, b"Gutter").unwrap_or(0);
                skip_element(reader, b"PAGEMARGIN")?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"PAGEDEF") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy PAGEDEF: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn parse_legacy_begin_number(e: &BytesStart, props: &mut DocProperties) {
    props.page_start_num = attr_u16(e, b"Page").unwrap_or(0);
    props.footnote_start_num = attr_u16(e, b"Footnote").unwrap_or(0);
    props.endnote_start_num = attr_u16(e, b"Endnote").unwrap_or(0);
    props.picture_start_num = attr_u16(e, b"Picture").unwrap_or(0);
    props.table_start_num = attr_u16(e, b"Table").unwrap_or(0);
    props.equation_start_num = attr_u16(e, b"Equation").unwrap_or(0);
}

fn parse_legacy_caret_pos(e: &BytesStart, props: &mut DocProperties) {
    props.caret_list_id = attr_u32(e, b"List").unwrap_or(0);
    props.caret_para_id = attr_u32(e, b"Para").unwrap_or(0);
    props.caret_char_pos = attr_u32(e, b"Pos").unwrap_or(0);
}

fn parse_legacy_font(e: &BytesStart, doc_info: &mut DocInfo, font_group: usize) {
    let name = attr_string(e, b"Name").unwrap_or_default();
    if name.is_empty() || font_group >= doc_info.font_faces.len() {
        return;
    }
    let alt_type = match attr_string(e, b"Type").as_deref() {
        Some("hft") | Some("HFT") => 2,
        Some("ttf") | Some("TTF") => 1,
        _ => 0,
    };
    doc_info.font_faces[font_group].push(Font {
        name,
        alt_type,
        ..Default::default()
    });
}

fn parse_legacy_bin_item(e: &BytesStart, doc_info: &mut DocInfo) {
    let storage_id = attr_u16(e, b"BinData").unwrap_or(0);
    let extension = attr_string(e, b"Format");
    if storage_id == 0 {
        return;
    }
    doc_info.bin_data_list.push(BinData {
        data_type: BinDataType::Embedding,
        storage_id,
        extension,
        ..Default::default()
    });
}

fn parse_legacy_bindata(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    out: &mut Vec<BinDataContent>,
) -> Result<(), HwpmlError> {
    let id = attr_u16(e, b"Id").unwrap_or(0);
    let text = read_plain_text(reader, b"BINDATA")?;
    if id == 0 {
        return Ok(());
    }
    let data = if matches!(
        attr_string(e, b"Encoding").as_deref(),
        Some("Base64") | Some("base64")
    ) {
        let compact: String = text.chars().filter(|ch| !ch.is_ascii_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(compact.as_bytes())
            .unwrap_or_default()
    } else {
        text.into_bytes()
    };
    out.push(BinDataContent {
        id,
        data,
        extension: String::new(),
    });
    Ok(())
}

fn parse_legacy_char_shape(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    doc_info: &mut DocInfo,
) -> Result<(), HwpmlError> {
    let mut cs = CharShape {
        base_size: attr_i32(e, b"Height").unwrap_or(1000),
        text_color: attr_u32(e, b"TextColor").unwrap_or(0),
        shade_color: attr_u32(e, b"ShadeColor").unwrap_or(0xFFFF_FFFF),
        kerning: attr_bool(e, b"UseKerning"),
        ..Default::default()
    };

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref ce)) | Ok(Event::Start(ref ce)) => {
                let qname = ce.name();
                let local = local_name(qname.as_ref());
                if eq_name(local, b"FONTID") {
                    parse_lang_u16s(ce, &mut cs.font_ids);
                } else if eq_name(local, b"RATIO") {
                    parse_lang_u8s(ce, &mut cs.ratios);
                } else if eq_name(local, b"CHARSPACING") {
                    parse_lang_i8s(ce, &mut cs.spacings);
                } else if eq_name(local, b"RELSIZE") {
                    parse_lang_u8s(ce, &mut cs.relative_sizes);
                } else if eq_name(local, b"CHAROFFSET") {
                    parse_lang_i8s(ce, &mut cs.char_offsets);
                } else if eq_name(local, b"BOLD") {
                    cs.bold = true;
                } else if eq_name(local, b"ITALIC") {
                    cs.italic = true;
                } else if eq_name(local, b"UNDERLINE") {
                    cs.underline_type = match attr_string(ce, b"Type").as_deref() {
                        Some("Top") => UnderlineType::Top,
                        Some("Bottom") | Some("Solid") => UnderlineType::Bottom,
                        _ => UnderlineType::Bottom,
                    };
                    cs.underline_color = attr_u32(ce, b"Color").unwrap_or(0);
                    cs.underline_shape = legacy_line_shape(attr_string(ce, b"Shape").as_deref());
                } else if eq_name(local, b"STRIKELINE") {
                    cs.strikethrough = true;
                    cs.strike_color = attr_u32(ce, b"Color").unwrap_or(0);
                    cs.strike_shape = legacy_line_shape(attr_string(ce, b"Shape").as_deref());
                }
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"CHARSHAPE") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy CHARSHAPE: {}", e))),
            _ => {}
        }
        buf.clear();
    }

    let idx = attr_u16(e, b"Id").unwrap_or(doc_info.char_shapes.len() as u16) as usize;
    set_vec_at(&mut doc_info.char_shapes, idx, cs);
    Ok(())
}

fn parse_legacy_tab_def(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    doc_info: &mut DocInfo,
) -> Result<(), HwpmlError> {
    let mut td = TabDef {
        auto_tab_left: attr_bool(e, b"AutoTabLeft"),
        auto_tab_right: attr_bool(e, b"AutoTabRight"),
        ..Default::default()
    };
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"TABITEM") => {
                td.tabs.push(TabItem {
                    position: attr_u32(ce, b"Pos").unwrap_or(0),
                    tab_type: match attr_string(ce, b"Type").as_deref() {
                        Some("Right") => 1,
                        Some("Center") => 2,
                        Some("Decimal") => 3,
                        _ => 0,
                    },
                    fill_type: legacy_leader(attr_string(ce, b"Leader").as_deref()),
                });
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"TABITEM") => {
                td.tabs.push(TabItem {
                    position: attr_u32(ce, b"Pos").unwrap_or(0),
                    tab_type: match attr_string(ce, b"Type").as_deref() {
                        Some("Right") => 1,
                        Some("Center") => 2,
                        Some("Decimal") => 3,
                        _ => 0,
                    },
                    fill_type: legacy_leader(attr_string(ce, b"Leader").as_deref()),
                });
                skip_element(reader, b"TABITEM")?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"TABDEF") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy TABDEF: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    let idx = attr_u16(e, b"Id").unwrap_or(doc_info.tab_defs.len() as u16) as usize;
    set_vec_at(&mut doc_info.tab_defs, idx, td);
    Ok(())
}

fn parse_legacy_tab_def_empty(e: &BytesStart, doc_info: &mut DocInfo) {
    let td = TabDef {
        auto_tab_left: attr_bool(e, b"AutoTabLeft"),
        auto_tab_right: attr_bool(e, b"AutoTabRight"),
        ..Default::default()
    };
    let idx = attr_u16(e, b"Id").unwrap_or(doc_info.tab_defs.len() as u16) as usize;
    set_vec_at(&mut doc_info.tab_defs, idx, td);
}

fn parse_legacy_numbering(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    doc_info: &mut DocInfo,
) -> Result<(), HwpmlError> {
    let mut num = Numbering {
        start_number: attr_u16(e, b"Start").unwrap_or(0),
        ..Default::default()
    };
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARAHEAD") => {
                let level = attr_u32(ce, b"Level").unwrap_or(1).saturating_sub(1) as usize;
                if level < 7 {
                    num.level_start_numbers[level] = attr_u32(ce, b"Start").unwrap_or(1);
                    num.heads[level] = NumberingHead {
                        char_shape_id: attr_u32(ce, b"CharShape").unwrap_or(0),
                        width_adjust: attr_i16(ce, b"WidthAdjust").unwrap_or(0),
                        text_distance: attr_i16(ce, b"TextOffset").unwrap_or(0),
                        number_format: legacy_num_format(attr_string(ce, b"NumFormat").as_deref()),
                        ..Default::default()
                    };
                }
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARAHEAD") => {
                let level = attr_u32(ce, b"Level").unwrap_or(1).saturating_sub(1) as usize;
                if level < 7 {
                    num.level_start_numbers[level] = attr_u32(ce, b"Start").unwrap_or(1);
                    num.heads[level] = NumberingHead {
                        char_shape_id: attr_u32(ce, b"CharShape").unwrap_or(0),
                        width_adjust: attr_i16(ce, b"WidthAdjust").unwrap_or(0),
                        text_distance: attr_i16(ce, b"TextOffset").unwrap_or(0),
                        number_format: legacy_num_format(attr_string(ce, b"NumFormat").as_deref()),
                        ..Default::default()
                    };
                    num.level_formats[level] = read_plain_text(reader, b"PARAHEAD")?;
                } else {
                    skip_element(reader, b"PARAHEAD")?;
                }
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"NUMBERING") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy NUMBERING: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    let idx = attr_u16(e, b"Id").unwrap_or(doc_info.numberings.len() as u16) as usize;
    set_vec_at(&mut doc_info.numberings, idx, num);
    Ok(())
}

fn parse_legacy_para_shape(
    e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    doc_info: &mut DocInfo,
) -> Result<(), HwpmlError> {
    let mut ps = ParaShape {
        alignment: legacy_alignment(attr_string(e, b"Align").as_deref()),
        tab_def_id: attr_u16(e, b"TabDef").unwrap_or(0),
        head_type: legacy_head_type(attr_string(e, b"HeadingType").as_deref()),
        numbering_id: attr_u16(e, b"Heading").unwrap_or(0),
        para_level: attr_u8(e, b"Level").unwrap_or(0),
        ..Default::default()
    };
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARAMARGIN") => {
                ps.indent = attr_i32(ce, b"Indent").unwrap_or(0);
                ps.margin_left = attr_i32(ce, b"Left").unwrap_or(0);
                ps.margin_right = attr_i32(ce, b"Right").unwrap_or(0);
                ps.spacing_before = attr_i32(ce, b"Prev").unwrap_or(0);
                ps.spacing_after = attr_i32(ce, b"Next").unwrap_or(0);
                ps.line_spacing_type = legacy_line_spacing_type(attr_string(ce, b"LineSpacingType").as_deref());
                ps.line_spacing = attr_i32(ce, b"LineSpacing").unwrap_or(0);
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARAMARGIN") => {
                ps.indent = attr_i32(ce, b"Indent").unwrap_or(0);
                ps.margin_left = attr_i32(ce, b"Left").unwrap_or(0);
                ps.margin_right = attr_i32(ce, b"Right").unwrap_or(0);
                ps.spacing_before = attr_i32(ce, b"Prev").unwrap_or(0);
                ps.spacing_after = attr_i32(ce, b"Next").unwrap_or(0);
                ps.line_spacing_type = legacy_line_spacing_type(attr_string(ce, b"LineSpacingType").as_deref());
                ps.line_spacing = attr_i32(ce, b"LineSpacing").unwrap_or(0);
                skip_element(reader, b"PARAMARGIN")?;
            }
            Ok(Event::Empty(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARABORDER") => {
                ps.border_fill_id = attr_u16(ce, b"BorderFill").unwrap_or(0);
                ps.border_spacing[0] = attr_i16(ce, b"OffsetLeft").unwrap_or(0);
                ps.border_spacing[1] = attr_i16(ce, b"OffsetRight").unwrap_or(0);
                ps.border_spacing[2] = attr_i16(ce, b"OffsetTop").unwrap_or(0);
                ps.border_spacing[3] = attr_i16(ce, b"OffsetBottom").unwrap_or(0);
            }
            Ok(Event::Start(ref ce)) if eq_name(local_name(ce.name().as_ref()), b"PARABORDER") => {
                ps.border_fill_id = attr_u16(ce, b"BorderFill").unwrap_or(0);
                ps.border_spacing[0] = attr_i16(ce, b"OffsetLeft").unwrap_or(0);
                ps.border_spacing[1] = attr_i16(ce, b"OffsetRight").unwrap_or(0);
                ps.border_spacing[2] = attr_i16(ce, b"OffsetTop").unwrap_or(0);
                ps.border_spacing[3] = attr_i16(ce, b"OffsetBottom").unwrap_or(0);
                skip_element(reader, b"PARABORDER")?;
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"PARASHAPE") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy PARASHAPE: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    let idx = attr_u16(e, b"Id").unwrap_or(doc_info.para_shapes.len() as u16) as usize;
    set_vec_at(&mut doc_info.para_shapes, idx, ps);
    Ok(())
}

fn parse_legacy_style(e: &BytesStart, doc_info: &mut DocInfo) {
    let style = Style {
        local_name: attr_string(e, b"Name").unwrap_or_default(),
        english_name: attr_string(e, b"EngName").unwrap_or_default(),
        style_type: match attr_string(e, b"Type").as_deref() {
            Some("Char") => 1,
            _ => 0,
        },
        next_style_id: attr_u8(e, b"NextStyle").unwrap_or(0),
        para_shape_id: attr_u16(e, b"ParaShape").unwrap_or(0),
        char_shape_id: attr_u16(e, b"CharShape").unwrap_or(0),
        ..Default::default()
    };
    let idx = attr_u8(e, b"Id").unwrap_or(doc_info.styles.len() as u8) as usize;
    set_vec_at(&mut doc_info.styles, idx, style);
}

fn parse_legacy_border_fill(
    _e: &BytesStart,
    reader: &mut Reader<&[u8]>,
    doc_info: &mut DocInfo,
) -> Result<(), HwpmlError> {
    let mut bf = BorderFill::default();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref ce)) => {
                let local = local_name(ce.name().as_ref()).to_ascii_uppercase();
                match local.as_slice() {
                    b"LEFTBORDER" => parse_legacy_border_line(ce, &mut bf, 0),
                    b"RIGHTBORDER" => parse_legacy_border_line(ce, &mut bf, 1),
                    b"TOPBORDER" => parse_legacy_border_line(ce, &mut bf, 2),
                    b"BOTTOMBORDER" => parse_legacy_border_line(ce, &mut bf, 3),
                    b"WINDOWBRUSH" => {
                        bf.fill.fill_type = FillType::Solid;
                        bf.fill.solid = Some(SolidFill {
                            background_color: attr_u32(ce, b"FaceColor").unwrap_or(0xFFFF_FFFF),
                            pattern_color: attr_u32(ce, b"HatchColor").unwrap_or(0),
                            pattern_type: 0,
                        });
                    }
                    _ => {}
                }
            }
            Ok(Event::Start(ref ce)) => {
                let local = local_name(ce.name().as_ref()).to_ascii_uppercase();
                match local.as_slice() {
                    b"LEFTBORDER" => parse_legacy_border_line(ce, &mut bf, 0),
                    b"RIGHTBORDER" => parse_legacy_border_line(ce, &mut bf, 1),
                    b"TOPBORDER" => parse_legacy_border_line(ce, &mut bf, 2),
                    b"BOTTOMBORDER" => parse_legacy_border_line(ce, &mut bf, 3),
                    b"WINDOWBRUSH" => {
                        bf.fill.fill_type = FillType::Solid;
                        bf.fill.solid = Some(SolidFill {
                            background_color: attr_u32(ce, b"FaceColor").unwrap_or(0xFFFF_FFFF),
                            pattern_color: attr_u32(ce, b"HatchColor").unwrap_or(0),
                            pattern_type: 0,
                        });
                    }
                    _ => {}
                }
                if local.as_slice() != b"BORDERFILL" {
                    skip_element(reader, local.as_slice())?;
                }
            }
            Ok(Event::End(ref ee)) if eq_name(local_name(ee.name().as_ref()), b"BORDERFILL") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("legacy BORDERFILL: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    let idx = attr_u16(_e, b"Id")
        .map(|id| id.saturating_sub(1) as usize)
        .unwrap_or(doc_info.border_fills.len());
    set_vec_at(&mut doc_info.border_fills, idx, bf);
    Ok(())
}

fn parse_legacy_border_line(e: &BytesStart, bf: &mut BorderFill, idx: usize) {
    if idx >= 4 {
        return;
    }
    bf.borders[idx].line_type = legacy_border_line_type(attr_string(e, b"Type").as_deref());
    bf.borders[idx].width = legacy_border_width(attr_string(e, b"Width").as_deref());
    bf.borders[idx].color = attr_u32(e, b"Color").unwrap_or(0);
}

fn append_char_shape_change(para: &mut Paragraph, char_shape_id: u32) {
    let pos = utf16_len(&para.text);
    if para
        .char_shapes
        .last()
        .is_none_or(|last| last.start_pos != pos || last.char_shape_id != char_shape_id)
    {
        para.char_shapes.push(CharShapeRef { start_pos: pos, char_shape_id });
    }
}

fn finalize_paragraph(para: &mut Paragraph) {
    para.char_offsets.clear();
    let mut utf16_pos = 0u32;
    for ch in para.text.chars() {
        para.char_offsets.push(utf16_pos);
        utf16_pos += if (ch as u32) > 0xFFFF { 2 } else { 1 };
    }
    para.char_count = utf16_pos + 1;
    if para.char_shapes.is_empty() {
        para.char_shapes.push(CharShapeRef { start_pos: 0, char_shape_id: 0 });
    }
    para.line_segs.push(LineSeg::default());
    if para.text.contains('\t') {
        para.control_mask |= 1 << 0x0009;
    }
    if para.text.contains('\n') {
        para.control_mask |= 1 << 0x000A;
    }
}

fn push_text(para: &mut Paragraph, text: &str) {
    para.text.push_str(text);
}

fn normalize_text(s: &str) -> String {
    s.replace('\u{00A0}', " ")
}

fn read_plain_text(reader: &mut Reader<&[u8]>, end_name: &[u8]) -> Result<String, HwpmlError> {
    let mut buf = Vec::new();
    let mut out = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(t)) => out.push_str(&String::from_utf8_lossy(t.as_ref())),
            Ok(Event::CData(t)) => out.push_str(&String::from_utf8_lossy(t.as_ref())),
            Ok(Event::GeneralRef(r)) => append_xml_ref(&r, &mut out),
            Ok(Event::End(ref e)) if eq_name(local_name(e.name().as_ref()), end_name) => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("text read: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn append_xml_ref(r: &quick_xml::events::BytesRef, out: &mut String) {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        out.push(ch);
        return;
    }
    if let Ok(name) = r.decode() {
        match name.as_ref() {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            "nbsp" => out.push('\u{00A0}'),
            _ => {
                out.push('&');
                out.push_str(&name);
                out.push(';');
            }
        }
    }
}

fn skip_element(reader: &mut Reader<&[u8]>, end_name: &[u8]) -> Result<(), HwpmlError> {
    let mut buf = Vec::new();
    let mut depth = 1u32;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if eq_name(local_name(e.name().as_ref()), end_name) => depth += 1,
            Ok(Event::End(ref e)) if eq_name(local_name(e.name().as_ref()), end_name) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("skip element: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn decode_xml(data: &[u8]) -> Result<String, HwpmlError> {
    let (text, had_errors) = if data.starts_with(&[0xFF, 0xFE]) {
        UTF_16LE.decode_without_bom_handling(&data[2..])
    } else if data.starts_with(&[0xFE, 0xFF]) {
        UTF_16BE.decode_without_bom_handling(&data[2..])
    } else if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        UTF_8.decode_without_bom_handling(&data[3..])
    } else {
        UTF_8.decode_without_bom_handling(data)
    };
    if had_errors {
        return Err(HwpmlError::XmlError("HWPML XML 인코딩 해석 실패".to_string()));
    }
    Ok(text.into_owned())
}

fn extract_first_subtree(xml: &str, target_local_name: &[u8]) -> Result<Option<String>, HwpmlError> {
    Ok(extract_all_subtrees(xml, target_local_name)?.into_iter().next())
}

fn extract_all_subtrees(xml: &str, target_local_name: &[u8]) -> Result<Vec<String>, HwpmlError> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if eq_name(local_name(e.name().as_ref()), target_local_name) => {
                out.push(capture_current_element(&mut reader, Event::Start(e.to_owned()))?);
            }
            Ok(Event::Empty(ref e)) if eq_name(local_name(e.name().as_ref()), target_local_name) => {
                out.push(write_single_event(Event::Empty(e.to_owned()))?);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(HwpmlError::XmlError(format!("HWPML subtree scan: {}", e))),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn capture_current_element(reader: &mut Reader<&[u8]>, start_event: Event<'static>) -> Result<String, HwpmlError> {
    let mut writer = Writer::new(Vec::new());
    writer.write_event(start_event)
        .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write(start): {}", e)))?;
    let mut depth = 1u32;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                depth += 1;
                writer.write_event(Event::Start(e.to_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::Empty(ref e)) => {
                writer.write_event(Event::Empty(e.to_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::Text(e)) => {
                writer.write_event(Event::Text(e.into_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::CData(e)) => {
                writer.write_event(Event::CData(e.into_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::Comment(e)) => {
                writer.write_event(Event::Comment(e.into_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::GeneralRef(e)) => {
                writer.write_event(Event::GeneralRef(e.into_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::PI(e)) => {
                writer.write_event(Event::PI(e.into_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::Decl(e)) => {
                writer.write_event(Event::Decl(e.into_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::DocType(e)) => {
                writer.write_event(Event::DocType(e.into_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write: {}", e)))?;
            }
            Ok(Event::End(ref e)) => {
                depth -= 1;
                writer.write_event(Event::End(e.to_owned()))
                    .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree write(end): {}", e)))?;
                if depth == 0 {
                    break;
                }
            }
            Ok(Event::Eof) => {
                return Err(HwpmlError::XmlError("HWPML subtree ended before matching close tag".to_string()));
            }
            Err(e) => return Err(HwpmlError::XmlError(format!("HWPML subtree read: {}", e))),
        }
        buf.clear();
    }
    String::from_utf8(writer.into_inner())
        .map_err(|e| HwpmlError::XmlError(format!("HWPML subtree UTF-8 변환 실패: {}", e)))
}

fn write_single_event(event: Event<'static>) -> Result<String, HwpmlError> {
    let mut writer = Writer::new(Vec::new());
    writer.write_event(event)
        .map_err(|e| HwpmlError::XmlError(format!("HWPML single-event write: {}", e)))?;
    String::from_utf8(writer.into_inner())
        .map_err(|e| HwpmlError::XmlError(format!("HWPML single-event UTF-8 변환 실패: {}", e)))
}

fn local_name(name: &[u8]) -> &[u8] {
    if let Some(pos) = name.iter().position(|&b| b == b':') {
        &name[pos + 1..]
    } else {
        name
    }
}

fn eq_name(actual: &[u8], expected: &[u8]) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

fn legacy_font_group(e: &BytesStart) -> usize {
    match attr_string(e, b"Lang").as_deref() {
        Some("Hangul") => 0,
        Some("Latin") => 1,
        Some("Hanja") => 2,
        Some("Japanese") => 3,
        Some("Other") => 4,
        Some("Symbol") => 5,
        Some("User") => 6,
        _ => 0,
    }
}

fn parse_lang_u16s(e: &BytesStart, out: &mut [u16; 7]) {
    out[0] = attr_u16(e, b"Hangul").unwrap_or(out[0]);
    out[1] = attr_u16(e, b"Latin").unwrap_or(out[1]);
    out[2] = attr_u16(e, b"Hanja").unwrap_or(out[2]);
    out[3] = attr_u16(e, b"Japanese").unwrap_or(out[3]);
    out[4] = attr_u16(e, b"Other").unwrap_or(out[4]);
    out[5] = attr_u16(e, b"Symbol").unwrap_or(out[5]);
    out[6] = attr_u16(e, b"User").unwrap_or(out[6]);
}

fn parse_lang_u8s(e: &BytesStart, out: &mut [u8; 7]) {
    out[0] = attr_u8(e, b"Hangul").unwrap_or(out[0]);
    out[1] = attr_u8(e, b"Latin").unwrap_or(out[1]);
    out[2] = attr_u8(e, b"Hanja").unwrap_or(out[2]);
    out[3] = attr_u8(e, b"Japanese").unwrap_or(out[3]);
    out[4] = attr_u8(e, b"Other").unwrap_or(out[4]);
    out[5] = attr_u8(e, b"Symbol").unwrap_or(out[5]);
    out[6] = attr_u8(e, b"User").unwrap_or(out[6]);
}

fn parse_lang_i8s(e: &BytesStart, out: &mut [i8; 7]) {
    out[0] = attr_i8(e, b"Hangul").unwrap_or(out[0]);
    out[1] = attr_i8(e, b"Latin").unwrap_or(out[1]);
    out[2] = attr_i8(e, b"Hanja").unwrap_or(out[2]);
    out[3] = attr_i8(e, b"Japanese").unwrap_or(out[3]);
    out[4] = attr_i8(e, b"Other").unwrap_or(out[4]);
    out[5] = attr_i8(e, b"Symbol").unwrap_or(out[5]);
    out[6] = attr_i8(e, b"User").unwrap_or(out[6]);
}

fn attr_string(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().eq_ignore_ascii_case(key))
        .map(|a| String::from_utf8_lossy(&a.value).to_string())
}

fn attr_bool(e: &BytesStart, key: &[u8]) -> bool {
    matches!(attr_string(e, key).as_deref(), Some("true") | Some("True") | Some("1"))
}

fn attr_u8(e: &BytesStart, key: &[u8]) -> Option<u8> {
    attr_string(e, key)?.parse().ok()
}

fn attr_i8(e: &BytesStart, key: &[u8]) -> Option<i8> {
    attr_string(e, key)?.parse().ok()
}

fn attr_u16(e: &BytesStart, key: &[u8]) -> Option<u16> {
    attr_string(e, key)?.parse().ok()
}

fn attr_i16(e: &BytesStart, key: &[u8]) -> Option<i16> {
    attr_string(e, key)?.parse().ok()
}

fn attr_u32(e: &BytesStart, key: &[u8]) -> Option<u32> {
    attr_string(e, key)?.parse().ok()
}

fn attr_i32(e: &BytesStart, key: &[u8]) -> Option<i32> {
    attr_string(e, key)?.parse().ok()
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

fn set_vec_at<T: Default + Clone>(vec: &mut Vec<T>, idx: usize, value: T) {
    if vec.len() <= idx {
        vec.resize(idx + 1, T::default());
    }
    vec[idx] = value;
}

fn legacy_alignment(v: Option<&str>) -> Alignment {
    match v.unwrap_or("Justify") {
        "Left" => Alignment::Left,
        "Right" => Alignment::Right,
        "Center" => Alignment::Center,
        "Distribute" => Alignment::Distribute,
        _ => Alignment::Justify,
    }
}

fn legacy_head_type(v: Option<&str>) -> HeadType {
    match v.unwrap_or("None") {
        "Outline" => HeadType::Outline,
        "Number" => HeadType::Number,
        "Bullet" => HeadType::Bullet,
        _ => HeadType::None,
    }
}

fn legacy_line_spacing_type(v: Option<&str>) -> LineSpacingType {
    match v.unwrap_or("Percent") {
        "Fixed" => LineSpacingType::Fixed,
        "SpaceOnly" => LineSpacingType::SpaceOnly,
        "Minimum" => LineSpacingType::Minimum,
        _ => LineSpacingType::Percent,
    }
}

fn legacy_border_line_type(v: Option<&str>) -> BorderLineType {
    match v.unwrap_or("Solid") {
        "None" => BorderLineType::None,
        "Dash" => BorderLineType::Dash,
        "Dot" => BorderLineType::Dot,
        "DashDot" => BorderLineType::DashDot,
        "DashDotDot" => BorderLineType::DashDotDot,
        "LongDash" => BorderLineType::LongDash,
        "Double" => BorderLineType::Double,
        _ => BorderLineType::Solid,
    }
}

fn legacy_border_width(v: Option<&str>) -> u8 {
    let s = v.unwrap_or("0.12mm");
    let mm = s.trim_end_matches("mm").parse::<f64>().unwrap_or(0.12);
    if mm <= 0.12 { 0 } else if mm <= 0.3 { 1 } else if mm <= 0.5 { 2 } else if mm <= 1.0 { 3 } else { 4 }
}

fn legacy_line_shape(v: Option<&str>) -> u8 {
    match v.unwrap_or("Solid") {
        "Dash" => 1,
        "Dot" => 2,
        "DashDot" => 3,
        "DashDotDot" => 4,
        "LongDash" => 5,
        "Circle" => 6,
        "Double" => 7,
        _ => 0,
    }
}

fn legacy_leader(v: Option<&str>) -> u8 {
    match v.unwrap_or("None") {
        "Solid" => 1,
        "Dot" => 2,
        "Dash" => 3,
        "DashDot" => 4,
        "DashDotDot" => 5,
        "LongDash" => 6,
        "Circle" => 7,
        _ => 0,
    }
}

fn legacy_num_format(v: Option<&str>) -> u8 {
    match v.unwrap_or("Digit") {
        "Digit" => 0,
        "HangulSyllable" => 1,
        _ => 0,
    }
}

fn parse_legacy_apply_page_type(v: Option<&str>) -> HeaderFooterApply {
    match v.unwrap_or("Both") {
        "Even" => HeaderFooterApply::Even,
        "Odd" => HeaderFooterApply::Odd,
        _ => HeaderFooterApply::Both,
    }
}

fn parse_legacy_text_wrap(v: Option<&str>) -> TextWrap {
    match v.unwrap_or("Square") {
        "Tight" => TextWrap::Tight,
        "Through" => TextWrap::Through,
        "TopAndBottom" => TextWrap::TopAndBottom,
        "BehindText" => TextWrap::BehindText,
        "InFrontOfText" => TextWrap::InFrontOfText,
        _ => TextWrap::Square,
    }
}

fn parse_legacy_vert_rel(v: Option<&str>) -> VertRelTo {
    match v.unwrap_or("Paper") {
        "Page" => VertRelTo::Page,
        "Para" => VertRelTo::Para,
        _ => VertRelTo::Paper,
    }
}

fn parse_legacy_horz_rel(v: Option<&str>) -> HorzRelTo {
    match v.unwrap_or("Paper") {
        "Page" => HorzRelTo::Page,
        "Column" => HorzRelTo::Column,
        "Para" => HorzRelTo::Para,
        _ => HorzRelTo::Paper,
    }
}

fn parse_legacy_vert_align(v: Option<&str>) -> VertAlign {
    match v.unwrap_or("Top") {
        "Center" => VertAlign::Center,
        "Bottom" => VertAlign::Bottom,
        "Inside" => VertAlign::Inside,
        "Outside" => VertAlign::Outside,
        _ => VertAlign::Top,
    }
}

fn parse_legacy_horz_align(v: Option<&str>) -> HorzAlign {
    match v.unwrap_or("Left") {
        "Center" => HorzAlign::Center,
        "Right" => HorzAlign::Right,
        "Inside" => HorzAlign::Inside,
        "Outside" => HorzAlign::Outside,
        _ => HorzAlign::Left,
    }
}

fn parse_legacy_num_type(v: Option<&str>) -> AutoNumberType {
    match v.unwrap_or("Page") {
        "Footnote" => AutoNumberType::Footnote,
        "Endnote" => AutoNumberType::Endnote,
        "Picture" | "Figure" => AutoNumberType::Picture,
        "Table" => AutoNumberType::Table,
        "Equation" => AutoNumberType::Equation,
        "TotalPage" | "Page" => AutoNumberType::Page,
        _ => AutoNumberType::Page,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_minimal_hwpml_document() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<hwpml xmlns:hh="http://www.hancom.co.kr/hwpml/2011/head"
       xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph"
       xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
  <hh:head version="1.2" secCnt="1">
    <hh:beginNum page="1" footnote="1" endnote="1" pic="1" tbl="1" equation="1"/>
    <hh:refList>
      <hh:fontfaces itemCnt="0"/>
      <hh:charProperties itemCnt="0"/>
      <hh:paraProperties itemCnt="0"/>
      <hh:styles itemCnt="0"/>
    </hh:refList>
  </hh:head>
  <hs:sec>
    <hp:p paraPrIDRef="0" styleIDRef="0">
      <hp:run charPrIDRef="0"><hp:t>안녕</hp:t></hp:run>
    </hp:p>
  </hs:sec>
</hwpml>"#;
        let doc = parse_hwpml(xml.as_bytes()).unwrap();
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.doc_properties.section_count, 1);
    }

    #[test]
    fn parses_legacy_minimal_hwpml_document() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<HWPML Version="2.1">
<HEAD SecCnt="1">
<DOCSETTING><BEGINNUMBER Page="1" Footnote="1" Endnote="1" Picture="1" Table="1" Equation="1"/></DOCSETTING>
<MAPPINGTABLE>
<FACENAMELIST><FONTFACE Lang="Hangul" Count="1"><FONT Id="0" Type="ttf" Name="바탕"/></FONTFACE></FACENAMELIST>
<CHARSHAPELIST Count="1"><CHARSHAPE Id="0" Height="1000" TextColor="0" ShadeColor="4294967295"><FONTID Hangul="0" Latin="0" Hanja="0" Japanese="0" Other="0" Symbol="0" User="0"/><RATIO Hangul="100" Latin="100" Hanja="100" Japanese="100" Other="100" Symbol="100" User="100"/><CHARSPACING Hangul="0" Latin="0" Hanja="0" Japanese="0" Other="0" Symbol="0" User="0"/><RELSIZE Hangul="100" Latin="100" Hanja="100" Japanese="100" Other="100" Symbol="100" User="100"/><CHAROFFSET Hangul="0" Latin="0" Hanja="0" Japanese="0" Other="0" Symbol="0" User="0"/></CHARSHAPE></CHARSHAPELIST>
<PARASHAPELIST Count="1"><PARASHAPE Id="0" Align="Justify" VerAlign="Baseline" HeadingType="None" Heading="0" Level="0" TabDef="0"><PARAMARGIN Indent="0" Left="0" Right="0" Prev="0" Next="0" LineSpacingType="Percent" LineSpacing="130"/><PARABORDER BorderFill="0" OffsetLeft="0" OffsetRight="0" OffsetTop="0" OffsetBottom="0"/></PARASHAPE></PARASHAPELIST>
<STYLELIST Count="1"><STYLE Id="0" Type="Para" Name="본문" EngName="Body" ParaShape="0" CharShape="0" NextStyle="0" LangId="1042"/></STYLELIST>
</MAPPINGTABLE>
</HEAD>
<BODY><SECTION Id="0"><P ParaShape="0"><TEXT CharShape="0"><SECDEF SpaceColumns="1134" TabStop="8000"><PAGEDEF Landscape="0" Width="59528" Height="84188"><PAGEMARGIN Left="4252" Right="4252" Top="5669" Bottom="4252" Header="3600" Footer="3600" Gutter="0"/></PAGEDEF></SECDEF><CHAR>안녕</CHAR></TEXT></P></SECTION></BODY>
</HWPML>"#;
        let doc = parse_hwpml(xml.as_bytes()).unwrap();
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].paragraphs.len(), 1);
        assert_eq!(doc.sections[0].paragraphs[0].text, "안녕");
    }
}
