pub trait MatchDocumentExt {
    fn is_image(&self) -> bool;

    fn is_document(&self) -> bool;
}

impl MatchDocumentExt for str {
    fn is_image(&self) -> bool {
        matches!(self, "png" | "jpg" | "jpeg" | "gif" | "webp")
    }

    fn is_document(&self) -> bool {
        matches!(
            self,
            "csv"
                | "doc"
                | "docx"
                | "epub"
                | "odp"
                | "ods"
                | "odt"
                | "pdf"
                | "ppt"
                | "pptx"
                | "rtf"
                | "xls"
                | "xlsb"
                | "xlsm"
                | "xlsx"
        )
    }
}
