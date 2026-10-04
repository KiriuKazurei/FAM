#include "previewpanel.h"

#include <QDir>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QLabel>
#include <QListWidget>
#include <QTextEdit>
#include <QVBoxLayout>

#include "theme.h"

namespace {
constexpr int kMaxTextPreviewBytes = 8 * 1024;
constexpr int kMaxTextPreviewLines = 100;
} /* namespace */

PreviewPanel::PreviewPanel(QWidget *parent) : QWidget(parent) {
    setObjectName(QString::fromLatin1(Theme::previewName()));

    m_header = new QLabel(this);
    m_header->setObjectName(QString::fromLatin1(Theme::previewHeaderName()));
    m_header->setWordWrap(true);

    m_meta = new QLabel(this);
    m_meta->setObjectName(QString::fromLatin1(Theme::previewMetaName()));
    m_meta->setWordWrap(true);

    m_list = new QListWidget(this);
    m_list->setObjectName(QString::fromLatin1(Theme::previewListName()));
    m_list->setSelectionMode(QAbstractItemView::NoSelection);
    m_list->setFrameShape(QFrame::NoFrame);
    m_list->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);

    m_content = new QTextEdit(this);
    m_content->setObjectName(QString::fromLatin1(Theme::previewContentName()));
    m_content->setReadOnly(true);
    m_content->setFrameShape(QFrame::NoFrame);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(Theme::instance().tokens().sm, Theme::instance().tokens().sm,
                               Theme::instance().tokens().sm, Theme::instance().tokens().sm);
    layout->addWidget(m_header);
    layout->addWidget(m_meta);
    layout->addWidget(m_list, 1);
    layout->addWidget(m_content, 1);
    m_list->hide();
    m_content->hide();

    clearPreview();
}

void PreviewPanel::setHeader(const QString &name, const QString &kind) {
    m_header->setText(QStringLiteral("%1  ·  %2").arg(name, kind));
}

void PreviewPanel::showDirectory(const QString &path) {
    const QFileInfo info(path);
    if (!info.isDir()) {
        clearPreview();
        return;
    }
    setHeader(info.fileName(), QStringLiteral("folder"));
    m_meta->setText(QDir::toNativeSeparators(path));
    m_list->clear();
    const auto entries = QDir(path).entryInfoList(
        QDir::AllEntries | QDir::NoDotAndDotDot, QDir::Name | QDir::DirsFirst);
    for (const QFileInfo &entry : entries) {
        const QString kind = entry.isDir() ? QStringLiteral("folder") : QStringLiteral("file");
        const QString size =
            entry.isDir() ? QStringLiteral("\xe2\x80\x94")
                          : QString::number(entry.size());
        m_list->addItem(QStringLiteral("%1  (%2, %3 B)").arg(entry.fileName(), kind, size));
    }
    m_meta->setText(QStringLiteral("%1\n%2 item(s)").arg(QDir::toNativeSeparators(path)).arg(entries.size()));
    m_list->show();
    m_content->hide();
}

void PreviewPanel::showFile(const QString &path) {
    const QFileInfo info(path);
    if (!info.isFile()) {
        clearPreview();
        return;
    }
    setHeader(info.fileName(), QStringLiteral("file"));
    m_meta->setText(QStringLiteral("%1\n%2 B  ·  %3")
                        .arg(QDir::toNativeSeparators(path))
                        .arg(info.size())
                        .arg(info.lastModified().toString(QStringLiteral("yyyy-MM-dd HH:mm"))));
    m_list->hide();

    /* Text preview: read the first N bytes and show them as a content hint. Non-text or over-size
     * files show metadata only — the content preview is a convenience, not a viewer. */
    QFile file(path);
    if (file.open(QIODevice::ReadOnly)) {
        const QByteArray head = file.read(kMaxTextPreviewBytes);
        // A null byte in the first chunk means binary: don't try to show it.
        if (!head.contains('\0')) {
            QString text = QString::fromUtf8(head);
            const QStringList lines = text.split(QLatin1Char('\n'));
            if (lines.size() > kMaxTextPreviewLines) {
                text = QStringList(lines.mid(0, kMaxTextPreviewLines)).join(QLatin1Char('\n'))
                    + QStringLiteral("\n… (preview truncated)");
            }
            m_content->setPlainText(text);
            m_content->show();
            return;
        }
    }
    m_content->hide();
}

void PreviewPanel::clearPreview() {
    setHeader(QStringLiteral("select a file or folder"), QString());
    m_meta->setText(QString());
    m_list->clear();
    m_list->hide();
    m_content->clear();
    m_content->hide();
}
