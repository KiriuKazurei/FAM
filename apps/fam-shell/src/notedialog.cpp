#include "notedialog.h"

#include <QDir>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QJsonObject>
#include <QLabel>
#include <QPushButton>
#include <QTextEdit>
#include <QVBoxLayout>

#include "bridge.h"
#include "theme.h"

namespace {

/* What the editor holds, in the bytes the store's limit counts. */
int utf8Bytes(const QString &text) {
    return text.toUtf8().size();
}

} /* namespace */

NoteDialog::NoteDialog(FamBridge *bridge, const QString &path, QWidget *parent)
    : QDialog(parent), m_bridge(bridge), m_path(path) {
    setWindowTitle(QStringLiteral("Notes — %1").arg(QFileInfo(path).fileName()));
    /* Named like every other surface, for the reason the tag dialog's first screenshot taught:
     * an unnamed QDialog falls back to the desktop's scheme, and every wire assertion in the run
     * still passes green. */
    setObjectName(QString::fromLatin1(Theme::pageName()));

    auto *heading = new QLabel(QDir::toNativeSeparators(path), this);
    heading->setToolTip(path);

    m_editor = new QTextEdit(this);
    m_editor->setObjectName(QString::fromLatin1(Theme::noteEditorName()));
    m_editor->setPlaceholderText(
        QStringLiteral("a note about this file — free text, not a tag, not searchable"));
    m_editor->setAcceptRichText(false);
    m_editor->setMinimumSize(420, 180);
    /* The 8 KiB wall, said where the typing happens rather than as an error afterwards. */
    m_editor->setToolTip(
        QStringLiteral("up to %1 bytes of UTF-8; the store refuses more")
            .arg(QString::number(8 * 1024)));

    auto *saveButton = new QPushButton(QStringLiteral("Save"), this);
    saveButton->setObjectName(QString::fromLatin1(Theme::primaryButtonName()));
    auto *clearButton = new QPushButton(QStringLiteral("Clear"), this);
    clearButton->setToolTip(
        QStringLiteral("Remove the note. Saving nothing is refused - the store will not read an "
                       "empty body as \"delete it for me\"."));

    m_line = new QLabel(this);
    m_line->setWordWrap(true);

    auto *buttons = new QHBoxLayout;
    buttons->addWidget(m_line, 1);
    buttons->addWidget(saveButton);
    buttons->addWidget(clearButton);

    auto *closeButton = new QPushButton(QStringLiteral("Close"), this);
    connect(closeButton, &QPushButton::clicked, this, &QDialog::accept);

    auto *layout = new QVBoxLayout(this);
    layout->addWidget(heading);
    layout->addWidget(m_editor, 1);
    layout->addLayout(buttons);
    layout->addWidget(closeButton, 0, Qt::AlignRight);

    connect(saveButton, &QPushButton::clicked, this, &NoteDialog::save);
    connect(clearButton, &QPushButton::clicked, this, &NoteDialog::clear);

    reload();
}

QString NoteDialog::editorText() const {
    return m_editor != nullptr ? m_editor->toPlainText() : QString();
}

void NoteDialog::setEditorText(const QString &text) {
    if (m_editor != nullptr) {
        m_editor->setPlainText(text);
    }
}

QString NoteDialog::noteLine() const {
    return m_line != nullptr ? m_line->text() : QString();
}

int NoteDialog::bodyBytes() const {
    return utf8Bytes(m_editor != nullptr ? m_editor->toPlainText() : QString());
}

void NoteDialog::showState() {
    if (m_body.isEmpty()) {
        m_line->setText(QStringLiteral("no note on this file"));
    } else {
        m_line->setText(QStringLiteral("last written %1").arg(m_updatedAt));
    }
}

void NoteDialog::reload() {
    m_body.clear();
    m_updatedAt.clear();
    QJsonValue result;
    QString error;
    if (!m_bridge->noteGet(m_path, &result, &error)) {
        m_line->setText(error);
        showState();
        return;
    }
    const QJsonObject object = result.toObject();
    m_body = object.value(QStringLiteral("body")).toString();
    m_updatedAt = object.value(QStringLiteral("updated_at")).toString();
    if (m_editor != nullptr) {
        m_editor->setPlainText(m_body);
    }
    showState();
}

bool NoteDialog::saveNote(QString *error) {
    const QString text = editorText();
    /* The two refusals the store would raise, raised here first: a blank body is not a note (and
     * must never be read as a clear), and the byte wall is the editor's business until the wire
     * confirms it. What reaches the store is the sentence both faces share. */
    if (text.trimmed().isEmpty()) {
        *error = QStringLiteral("a note must say something; use Clear to remove one");
        return false;
    }
    const int bytes = utf8Bytes(text);
    if (bytes > 8 * 1024) {
        *error = QStringLiteral("a note on %1 would be %2 bytes; the limit is %3")
                     .arg(QFileInfo(m_path).fileName())
                     .arg(bytes)
                     .arg(8 * 1024);
        return false;
    }
    QJsonValue result;
    if (!m_bridge->noteSet(m_path, text, &result, error)) {
        return false;
    }
    m_changed = true;
    reload();
    return true;
}

bool NoteDialog::clearNote(QString *error) {
    QJsonValue result;
    if (!m_bridge->noteClear(m_path, &result, error)) {
        return false;
    }
    const bool removed = result.toObject().value(QStringLiteral("cleared")).toBool();
    if (removed) {
        m_changed = true;
    }
    reload();
    return removed;
}

void NoteDialog::save() {
    QString error;
    if (!saveNote(&error)) {
        m_line->setText(error);
        return;
    }
}

void NoteDialog::clear() {
    QString error;
    if (!clearNote(&error)) {
        /* The one non-error a clear can produce: there was nothing to drop. Said, not silently
         * swallowed - the person pressed a button and deserves to know it did nothing. */
        m_line->setText(QStringLiteral("there was no note to clear"));
        return;
    }
}
