#ifndef NOTEDIALOG_H
#define NOTEDIALOG_H

#include <QDialog>
#include <QString>

class FamBridge;
class QLabel;
class QPushButton;
class QTextEdit;

/* The free-text note on one file: read it, write it, take it off.
 *
 * This dialog exists for the same reason the tag dialog did: the note has been in the store since
 * schema v2 and in the agent's tool set since increment 12, and the human face had nothing - not
 * even an ABI entry to call. A person and an agent could look at the same file and only one of
 * them could see what the other had written.
 *
 * What a note is not, enforced everywhere below: it is not a tag (no vocabulary, no source, no
 * place in a tag list), it is not searchable (the body never enters the index, so `note.get` on
 * the exact path is the only way back), and an empty body is not a clear - the store refuses it,
 * and this dialog makes the refusal the person's first-class gesture instead (`Clear` removes the
 * note; `Save` with nothing to say is told to say something). */
class NoteDialog final : public QDialog {
    Q_OBJECT

public:
    NoteDialog(FamBridge *bridge, const QString &path, QWidget *parent = nullptr);

    /* The operations, callable without the dialog ever being shown - the same seam the tag dialog
     * opened, because a modal is exactly the thing a run with no hands cannot drive. */
    /* What the editor holds right now, and what the store held at the last reload. */
    QString editorText() const;
    void setEditorText(const QString &text);
    /* The store's answer as of the last reload: the body, or an empty string when there is none. */
    QString storedNote() const { return m_body; }
    /* When the store last wrote it, or empty for "no note". */
    QString updatedAt() const { return m_updatedAt; }
    /* Writes the editor's text. Refuses the same blanks the store does, before crossing the wire,
     * with the store's own sentence; `error` carries whichever refusal happened. */
    bool saveNote(QString *error);
    /* Drops the note. True when there was one, so "cleared" and "there was nothing" stay apart. */
    bool clearNote(QString *error);
    /* Re-reads the store into the editor. */
    void reload();
    /* True once anything was saved or cleared. */
    bool changed() const { return m_changed; }
    /* What the note line says right now - the refusal or the "when", for the headless log. */
    QString noteLine() const;

private slots:
    void save();
    void clear();

private:
    void showState();
    /* UTF-8 bytes, which is what the store's limit counts. */
    int bodyBytes() const;

    FamBridge *m_bridge;
    QString m_path;
    QTextEdit *m_editor = nullptr;
    QLabel *m_line = nullptr;
    QString m_body;
    QString m_updatedAt;
    bool m_changed = false;
};

#endif /* NOTEDIALOG_H */
