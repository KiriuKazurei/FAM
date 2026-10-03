#ifndef TAGMANAGERDIALOG_H
#define TAGMANAGERDIALOG_H

#include <QDialog>
#include <QString>
#include <QStringList>

class FamBridge;
class QLabel;
class QLineEdit;
class QListWidget;
class QPushButton;

/* Every label in the library, with the browsed directory's own labels first - and the three edits
 * a label can need: rename it (which with an absent target is just a rename), merge it into
 * another (the same call, the other reading), or delete it everywhere.
 *
 * The dialog is modal like its siblings, and its operations are public for the same reason: a run
 * with no hands drives them through the same code a click reaches. Delete is two-step - the name
 * leaves every file at once, which is the one action here that no cascade guards. */
class TagManagerDialog final : public QDialog {
    Q_OBJECT

public:
    TagManagerDialog(FamBridge *bridge, const QString &currentDir, QWidget *parent = nullptr);

    /* What the list holds right now: every name, current-directory ones first. */
    QStringList tagNames() const;
    /* The selected tag, or empty. */
    QString selectedTag() const;
    QString entryText() const;
    void setEntryText(const QString &text);
    /* Rename/merge the selected tag into `target`. The store's moved/collapsed/files answer is
     * the story either way - "rename" and "merge" are one operation seen from two distances. */
    bool applyMerge(const QString &target, QString *error);
    /* Delete the selected name everywhere; two-step (the first call arms). True when rows went. */
    bool deleteTag(QString *error);
    void reload();
    bool changed() const { return m_changed; }
    QString noteLine() const;
    bool selectFirstRow();

private slots:
    void applyClicked();
    void deleteClicked();

private:
    void showList();
    void disarmDelete();
    QString selectedName() const;

    FamBridge *m_bridge;
    QString m_currentDir;
    QListWidget *m_list = nullptr;
    QLineEdit *m_entry = nullptr;
    QLabel *m_line = nullptr;
    QPushButton *m_deleteButton = nullptr;
    QStringList m_names;
    /* The raw tag names behind the display strings - what the ops pass to the store. */
    QStringList m_rawNames;
    bool m_changed = false;
    bool m_deleteArmed = false;
};

#endif /* TAGMANAGERDIALOG_H */
