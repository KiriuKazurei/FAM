#ifndef PREVIEWPANEL_H
#define PREVIEWPANEL_H

#include <QWidget>

class QLabel;
class QListWidget;
class QTextEdit;

/* The right-hand panel that follows the selection: what a click means when clicking no longer
 * navigates. A directory's children are listed (name + size); a file's metadata and, for text
 * files, a content preview are shown.
 *
 * The reads here are shell-side (QDir/QFile) — the ABI's watching and audit are for the browsing
 * columns. The preview is a passive snapshot that refreshes when the selection changes, not a
 * data-access path. */
class PreviewPanel final : public QWidget {
    Q_OBJECT

public:
    explicit PreviewPanel(QWidget *parent = nullptr);

    void showDirectory(const QString &path);
    void showFile(const QString &path);
    void clearPreview();

private:
    void setHeader(const QString &name, const QString &kind);
    QLabel *m_header = nullptr;
    QLabel *m_meta = nullptr;
    QListWidget *m_list = nullptr;
    QTextEdit *m_content = nullptr;
};

#endif /* PREVIEWPANEL_H */
